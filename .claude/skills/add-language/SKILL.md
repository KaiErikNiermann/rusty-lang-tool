---
name: add-language
description: Add a new language (L1 spelling + L2 grammar) to rusty-lang-tool end-to-end, reusing the per-language LangConfig seam. Use when the user asks to add/port a language (e.g. "add Italian", "add fr"). Covers the gated procedure, the accelerator (xtask lang-inspect), the upstream-sync manifest, and the standing directive to watch for per-script generalizations.
---

# Adding a language to rusty-lang-tool

The pipeline is per-language data behind one seam (`crates/rlt-lang/src/lib.rs` `LangConfig`). en, de, ru, ar are done. Adding a language is mostly **data, not code** — a `LangConfig` const + a `config()` arm + a `SPARSE_PATHS` entry + built artifacts. The only genuinely new code appears when a far script breaks a Latin/ASCII assumption (see the **Standing directive** below).

## STANDING DIRECTIVE — watch for per-script generalizations

During every language addition, when the new script breaks an `is_ascii_*` / `is_alphanumeric` / `is_ascii_digit`-shaped assumption, prefer a **category-based, library-backed (`unicode-properties`), provably-en/de/ru/ar-non-regressing** generalization over a per-language special-case — and add a unit test proving the byte-identical gate for the existing languages. Already generalized this way:
- per-language **alphabet** (`SpellConfig.alphabet`; ru Cyrillic, ar Arabic base letters);
- dict **encoding** (`fsa.dict.encoding` via `encoding_rs`; ru KOI8-R, it ISO-8859-15) and **FSA format** (CFSA2 `0xc6` + FSA5 `0x05`, dispatched in the morfologik reader; it FSA5) and **repo-vs-Maven** dict source (`PosDict::{Maven,Repo}`);
- combining-mark **word-internality + lookup normalization** (`is_word_char` Mn clause + `Normalization::StripCombiningMarks`; ar tashkeel);
- **Unicode-Nd digits** (`is_decimal_digit`; ar Arabic-Indic);
- morfologik **`frequency-included`** tag-byte stripping (fr/es) and **apostrophe elision** (`LangConfig.elision`; fr/it `l'`/`dell'`).

If the new language needs none of these, it is pure data. If it needs a NEW one, generalize at the right altitude and record it here.

## Procedure (each step is a gate)

1. **Sparse path + fetch.** Add `languagetool-language-modules/<m>/src/main/resources/org/languagetool` to `SPARSE_PATHS` (`xtask/src/main.rs`); run `cargo xtask fetch-lt`. **Gate:** the `resources/lt/_repo/.../<m>/` tree exists.

2. **Inspect (the accelerator).** Run `cargo xtask lang-inspect --code <code>` (works before a config exists; for a Maven dict it auto-fetches once the config names the coords, else pass `--dict`/`--info`). It prints: FSA version byte (`0xc6` CFSA2 or `0x05` FSA5 — both supported; anything else stops), `.info` separator/encoder/encoding, triple/tag counts, sample entries, the **vocalized verdict** (→ whether `Normalization::StripCombiningMarks` is needed), the **spell alphabet** (distinct base letters), grammar/disambig **candidate postags**, and the **confusion-pair count**. **Gate:** capture all of these — they fill the config and decide L3.

3. **Author the `LangConfig`.** Add a `static XX` const + a `config()` arm in `crates/rlt-lang/src/lib.rs`. Fields from inspect:
   - `pos_dict`: `PosDict::Repo { dict_file, info_file }` if the dict ships in the repo (ru/ar), else `PosDict::Maven {…}` (en/de — verify the artifact/version).
   - `tagset`: `digit_tag`/`punctuation_tag`/`proper_noun_tag` from the most-referenced postags; `oov_tag` = `"UNKNOWN"`, `sent_start`/`sent_end` universal. `punctuation_chars` = the shared set plus any script-specific marks (e.g. Arabic `،؛؟`).
   - `sources`: usually all false (`confusion:true` only if inspect shows ≥1 confusion pair).
   - `spell.alphabet`: the inspect-derived base letters (no combining marks).
   - `normalization`: `StripCombiningMarks` iff inspect reports an unvocalized dict for a mark-using script, else `None`.
   - `compounds`: `None` unless the language productively compounds like German.
   Update the `lang_cfg`/CLI/wasm "known languages" error strings.

4. **Build artifacts.** `cargo xtask build-lang --lang <code>` (tagger + disambig + grammar blob); optionally `build-confusion` if `confusion:true`. **Gate:** builds; triple count sane; record grammar coverage / opaque %.

5. **Smoke + oracle + tests.** `cargo run -p rlt-cli -- tokens --lang <code> "<sentence>"` (eyeball tags, incl. any normalization/digit behaviour). Add `XX_native_reproduces_examples` to `crates/rlt-cli/tests/oracle.rs` (mirror ru/ar) — run it, set floors **just below** the first measured reproduction/FP. Add `engine_analyze_XX.rs` fuzz target + a morfologik dict test. **Gate:** the new oracle passes; **en/de/ru/ar oracle floors UNCHANGED** (the byte-identical gate); fuzz + Miri (`-Zmiri-tree-borrows`, add `-Zmiri-disable-isolation` for fixture-reading tests) clean.

6. **Pin the manifest.** `cargo xtask lang-manifest --lang <code>` → commit `lang-manifests/<code>.json`. **Gate:** `cargo xtask lang-status --lang <code>` reports all-unchanged.

7. **Verify the workspace.** `cargo clippy --workspace --all-targets` clean; `cargo build -p rlt-wasm --target wasm32-unknown-unknown` compiles. Commit atomically (accelerator / generalizations / language+tests / manifest).

## Keeping languages in sync with upstream

`lang-manifests/<code>.json` records SHA-256 of every upstream input (grammar.xml, dict, disambiguation.xml, added/removed, confusion_sets) at the pinned `LT_VERSION`. To check for **linguistic** drift (not just version): bump `LT_VERSION` (or `RLT_LT_VERSION=vX.Y`) → `cargo xtask fetch-lt` → `cargo xtask lang-status` shows exactly which files changed. Validate the rebuild (oracle floors), then `cargo xtask lang-manifest --lang <code>` to re-pin. `lang-status` (no `--lang`) covers all languages and exits non-zero on drift — CI-gateable.
