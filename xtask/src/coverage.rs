//! `coverage`: compare our pipeline with LanguageTool itself on LT's `<example>` sentences.
//!
//! Resolves the pinned LanguageTool release from Maven Central into `tools/lt-runtime/` (Gradle,
//! once per version and language), runs `tools/lt-probe/LtProbe.java` over the examples (cached by
//! a hash of its inputs), and hands both sides to [`rlt_cli::coverage::compare`]. Writes
//! `target/coverage/<lang>/report.json` and `outcomes.jsonl` and prints a summary. Needs `java`
//! (21+) and `gradle` on `PATH`, `fetch-lt` and the language's built artifacts.

use std::fmt::Write as _;
use std::io::{BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use rlt_cli::coverage::{self, CauseCount, CoverageReport, LtSentence};
use rlt_convert::Example;

const PROBE_DIR: &str = "tools/lt-probe";
const PROBE_SRC: &str = "tools/lt-probe/LtProbe.java";
const RUNTIME_DIR: &str = "tools/lt-runtime";
/// Rules listed per cause in the printed summary (the JSON report has all of them).
const TOP_RULES: usize = 5;

/// Run the comparison for `cfg` and print the summary.
pub fn run(cfg: &'static rlt_lang::LangConfig, lt_version: &str) -> Result<()> {
    let version = lt_version.trim_start_matches('v');
    let out_dir = PathBuf::from(format!("target/coverage/{}", cfg.code));
    std::fs::create_dir_all(&out_dir)?;

    let grammar = cfg.grammar_xml_path();
    let examples = rlt_convert::extract_examples(Path::new(&grammar))
        .with_context(|| format!("reading {grammar} (run `cargo xtask fetch-lt`)"))?;
    let rules =
        rlt_ir::deserialize_rules(&std::fs::read(cfg.grammar_blob_path()).with_context(|| {
            format!(
                "reading {} (run `cargo xtask build-lang --lang {}`)",
                cfg.grammar_blob_path(),
                cfg.code
            )
        })?)
        .map_err(|e| anyhow::anyhow!("decoding {}: {e}", cfg.grammar_blob_path()))?;

    let runtime = ensure_runtime(version, cfg.lt_module)?;
    let lt = probe(&runtime, cfg.code, version, &examples, &out_dir)?;
    let engine = rlt_native::NativeEngine::for_lang(cfg)
        .map_err(|e| anyhow::anyhow!("loading the {} engine (build-lang first): {e}", cfg.code))?;
    let synthesizer = rlt_cli::oracle_score::load_synthesizer(cfg)?;
    println!(
        "synthesizer: {}",
        if synthesizer.is_some() {
            "loaded"
        } else {
            "none (suggestions that need inflected forms are rendered without them)"
        }
    );
    let report = coverage::compare(
        &engine,
        &rules,
        &examples,
        &lt,
        cfg.tagset.sent_start,
        cfg.tagset.sent_end,
        synthesizer.as_ref(),
    );

    write_outputs(&report, &out_dir)?;
    print!("{}", summary(cfg.code, version, &report));
    println!(
        "details: {}/report.json, {}/outcomes.jsonl",
        out_dir.display(),
        out_dir.display()
    );
    Ok(())
}

/// The Maven runtime for `version`/`module`, resolved with Gradle on first use.
fn ensure_runtime(version: &str, module: &str) -> Result<PathBuf> {
    let dir = PathBuf::from(format!("{RUNTIME_DIR}/{version}/{module}"));
    let done = dir.join(".complete");
    if done.exists() {
        return Ok(dir);
    }
    println!(
        "resolving LanguageTool {version} (language-{module}) into {}",
        dir.display()
    );
    let out_dir = std::fs::canonicalize(".")?.join(&dir);
    let status = Command::new("gradle")
        .args(["-q", "--no-daemon", "-p", PROBE_DIR, "copyDeps"])
        .arg(format!("-PltVersion={version}"))
        .arg(format!("-PltLang={module}"))
        .arg(format!("-PoutDir={}", out_dir.display()))
        .status()
        .context("spawning `gradle` (is it installed?)")?;
    if !status.success() {
        bail!("gradle could not resolve org.languagetool:language-{module}:{version}");
    }
    std::fs::write(&done, b"")?;
    Ok(dir)
}

/// LT's verdict on every example, from the cache when the probe, release and examples are unchanged.
fn probe(
    runtime: &Path,
    code: &str,
    version: &str,
    examples: &[Example],
    out_dir: &Path,
) -> Result<Vec<LtSentence>> {
    let mut input = String::new();
    for (i, ex) in examples.iter().enumerate() {
        writeln!(
            input,
            "{}",
            serde_json::json!({"id": i.to_string(), "text": ex.text})
        )?;
    }
    let mut keyed = format!("{version}\n{code}\n").into_bytes();
    keyed.extend(std::fs::read(PROBE_SRC)?);
    keyed.extend(input.as_bytes());
    let key = crate::sha256_bytes(&keyed).0;

    let cached = out_dir.join("lt-probe.jsonl");
    let key_file = out_dir.join("lt-probe.key");
    if std::fs::read_to_string(&key_file).is_ok_and(|k| k == key) {
        println!("LanguageTool results: cached ({})", cached.display());
    } else {
        println!(
            "running LanguageTool {version} on {} examples…",
            examples.len()
        );
        run_probe(runtime, code, &input, &cached)?;
        std::fs::write(&key_file, &key)?;
    }
    let rows: Vec<LtSentence> = std::io::BufReader::new(std::fs::File::open(&cached)?)
        .lines()
        .map(|l| Ok(serde_json::from_str(&l?)?))
        .collect::<Result<_>>()?;
    if rows.len() != examples.len() {
        bail!(
            "{} has {} rows for {} examples",
            cached.display(),
            rows.len(),
            examples.len()
        );
    }
    Ok(rows)
}

/// Run the probe with `input` on stdin, writing its output to `out` (via a temporary file, so an
/// interrupted run never leaves a partial cache behind).
fn run_probe(runtime: &Path, code: &str, input: &str, out: &Path) -> Result<()> {
    let partial = out.with_extension("jsonl.partial");
    let mut child = Command::new("java")
        .arg("-Xmx4g")
        .arg("-cp")
        .arg(runtime.join("*"))
        .arg(PROBE_SRC)
        .arg(code)
        .stdin(Stdio::piped())
        .stdout(std::fs::File::create(&partial)?)
        .spawn()
        .context("spawning `java` (is a JDK 21+ installed?)")?;
    child
        .stdin
        .take()
        .context("probe stdin")?
        .write_all(input.as_bytes())?;
    let status = child.wait()?;
    if !status.success() {
        bail!("the LanguageTool probe exited with {status}");
    }
    std::fs::rename(&partial, out)?;
    Ok(())
}

fn write_outputs(report: &CoverageReport, out_dir: &Path) -> Result<()> {
    let mut f = BufWriter::new(std::fs::File::create(out_dir.join("outcomes.jsonl"))?);
    for o in &report.outcomes {
        serde_json::to_writer(&mut f, o)?;
        f.write_all(b"\n")?;
    }
    f.flush()?;
    let summary = serde_json::json!({
        "totals": report.totals,
        "gaps": report.gaps,
        "false_positives": report.false_positives,
    });
    std::fs::write(
        out_dir.join("report.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    Ok(())
}

#[allow(
    clippy::cast_precision_loss,
    reason = "example counts are far below f64's exact-integer range"
)]
fn pct(n: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        n as f64 * 100.0 / total as f64
    }
}

fn causes<C: std::fmt::Display>(out: &mut String, title: &str, list: &[CauseCount<C>]) {
    let total: usize = list.iter().map(|c| c.count).sum();
    let _ = writeln!(out, "\n{title}: {total}");
    for c in list {
        let top: Vec<String> = c
            .rules
            .iter()
            .take(TOP_RULES)
            .map(|(r, n)| format!("{r} {n}"))
            .collect();
        let _ = writeln!(out, "  {:5}  {}", c.count, c.cause);
        let _ = writeln!(out, "         {}", top.join(", "));
    }
}

/// The printed report.
fn summary(code: &str, version: &str, r: &CoverageReport) -> String {
    let t = &r.totals;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "== {code}: our pipeline vs LanguageTool {version} on its examples =="
    );
    let _ = writeln!(
        out,
        "reproduced ({} incorrect examples): LT grammar.xml rules {} ({:.1}%), any LT rule {}, \
         ours {} ({:.1}%), ours on LT's analysis {}; ours and not LT {}",
        t.positives,
        t.lt_grammar,
        pct(t.lt_grammar, t.positives),
        t.lt_any,
        t.ours,
        pct(t.ours, t.positives),
        t.ours_on_lt,
        t.ours_not_lt,
    );
    let _ = writeln!(
        out,
        "false positives ({} correct examples): LT {}, ours {}, ours on LT's analysis {}",
        t.negatives, t.lt_false_positives, t.ours_false_positives, t.ours_on_lt_false_positives,
    );
    causes(&mut out, "LT reproduces, we do not", &r.gaps);
    causes(&mut out, "we fire, LT does not", &r.false_positives);
    out
}
