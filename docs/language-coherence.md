# Language coherence

A language touches a dozen places: a `LangConfig`, a sparse-checkout path, a content manifest, a CI
matrix, a handful of tests. The failure mode is *partial* wiring — a CI script that scores 7 languages,
a fuzz file that rotates through 6, and a test file that covers 8 — with nothing flagging the gap. The
coherence system makes that gap impossible (for the sites that can share Rust) or loud (for the sites
that can't).

## The canonical source

[`rlt_lang::LANGUAGES`](../crates/rlt-lang/src/lib.rs) is the single ordered list of configured
languages. Adding a language is appending its `LangConfig` `static` there. Everything that *can* read a
Rust const derives from it, so those sites cannot drift:

| Site | Derives from `LANGUAGES` via |
| --- | --- |
| `config(code)` lookup | `LANGUAGES.iter().find(...)` |
| `known()` — the "unknown language (known: …)" help text | `LANGUAGES.iter().map(\|c\| c.code)` |
| rlt-cli `resolve_lang` error | `rlt_lang::known()` |
| xtask `lang_cfg` error | `rlt_lang::known()` |
| xtask `configured_langs()` (the `lang-status` default set) | `rlt_lang::LANGUAGES` |
| fuzz `codes()` (the `engine_analyze` rotation) | `rlt_lang::LANGUAGES` |

There is no second array of codes anywhere. If you find yourself writing `["en", "de", …]`, derive it
from `LANGUAGES` instead.

## The checker — `cargo xtask lang-coherence`

Some sites can't share the Rust const: the per-language content manifest is JSON, the CI matrix is YAML,
and the tests are matched by *name*. For those, `lang-coherence` verifies — per language — that the site
includes it, and **exits non-zero** on any required gap. It runs in the CI `lint` job (no fetched data
needed; it only reads committed files).

Required checks (each gates CI):

| Check | Site | What it verifies |
| --- | --- | --- |
| `manifest` | `lang-manifests/<code>.json` | the upstream content-hash manifest exists |
| `sparse-checkout path` | `SPARSE_PATHS` in `xtask/src/main.rs` | `fetch-lt` pulls the language's resources (matches `LangConfig::lt_sparse_path`) |
| `nightly oracle matrix` | `lang:` list in `.github/workflows/nightly.yml` | the language is scored in nightly CI |
| `morfologik dict test` | `reads_real_languagetool_<name>_dict` in `rlt-convert/src/morfologik.rs` | the real LT dict reads back |
| `native oracle test` | `<code>_native_reproduces_examples` in `rlt-cli/tests/oracle.rs` | L2 grammar reproduces LT's `<example>`s (English is covered by the `nlprule_baseline` + `ir_matcher` pair instead) |
| `L3 confusion build` | `confusion_corpus(code)` in `xtask/src/main.rs` | if `sources.confusion`, the L3 model is buildable (English builds via the dedicated Norvig path) |

Recommended check (warns, never fatal):

| Check | Site | Note |
| --- | --- | --- |
| `L3 oracle floor` | `<code>_l3_confusion_precision_recall` in `rlt-cli/tests/oracle.rs` | a language may legitimately enable L3 without a scored floor — Russian's corpus is too small for a meaningful precision/recall number |

Finally, the checker sweeps the tree for numeric `N lang…` mentions whose `N` differs from the configured
count and lists them (informational only — phrases like "the 3 romance langs" are legitimate subsets, so
this never gates CI; it just surfaces stale prose like a comment claiming the wrong total).

## Adding a language — the coherence checklist

`lang-coherence` is the final gate after wiring a new language. Work it until it's green:

1. Add the `LangConfig` `static` and append it to `LANGUAGES` (`crates/rlt-lang/src/lib.rs`), with the
   `name` field set to the language's lower-cased English name (`english`, `german`, …) — the checker
   keys the morfologik test name off it.
2. Add the sparse-checkout path to `SPARSE_PATHS` (`xtask/src/main.rs`) and run `fetch-lt`.
3. Build artifacts: `cargo xtask build-lang --lang <code>` (plus `build-confusion` if L3).
4. Add the tests the checker looks for: the morfologik dict test, the native oracle test, and — if L3 —
   a `confusion_corpus` arm (and ideally the L3 floor test).
5. Add the language to the nightly oracle `lang:` matrix.
6. Pin the upstream content: `cargo xtask lang-manifest --lang <code>`.
7. `cargo xtask lang-coherence` — every required check PASS, exit 0. Now you know the language is wired
   into *all* systems, not just the ones you remembered.
