# rusty-lang-tool (`rlt`)

A **fully local, web-native grammar and spell checker** that reuses
[LanguageTool](https://github.com/languagetool-org/languagetool)'s open rule corpus but runs
entirely client-side (native or WebAssembly) — no server, no subscription, no telemetry.

The bet: the value in this domain is LanguageTool's ~20 years of hand-authored multilingual rule
data, *not* engine code. Every prior Rust reuse of it (notably
[nlprule](https://github.com/bminixhofer/nlprule)) went dormant because its converter was welded
to one LT release's format. `rlt`'s answer is a **self-maintaining converter**: generate the XML
parser from LT's own XSD schemas, and use LT's ~22,666 bundled `<example>` sentences as a
differential oracle — so tracking a new LT release is "run the harness, triage the red".

## Status

MVP complete (English): **L1 spelling + L2 grammar**, as a Rust crate + CLI with a working
`wasm32` build that runs in Node. Two L2 backends share one differential oracle over LT's ~9k
bundled `<example>` sentences:

- **nlprule baseline** (LT v5.2 rules): reproduces **55.3%**.
- **IR matcher** (our converter's LT **v6.7** rules): reproduces **58.5%** — the on-thesis path,
  ahead of the baseline, at 6.2% false-positive rate. Handles tokens/`<or>`/`<and>`/`<marker>`/
  `<exception>`/`<antipattern>`/`<phraseref>`/rule-level `<regexp>` and `<match>` `regexp_replace`
  transforms; `<unify>` is unused in English and `postag_replace` synthesis is out of scope.
- **L3 confusion** (real-word errors, e.g. their/there): **82.6%** recall on perturbed sentences,
  via a bigram + POS-context likelihood ratio over Norvig's n-grams pruned to LT's confusion sets.
- **L4 neural** (long-tail grammar): a GECToR edit-tagger, **int8-quantized and run in pure Rust via
  `rten`** — **native and in wasm** — with GECToR-style iterative refinement. `rlt check "She go to
  school"` → `"go" → "goes"`; composes onto L1–L3 behind the same trait. **F0.5 = 0.545** on BEA-2019
  dev (ERRANT). Non-commercial (see Licensing).

## Architecture (the cascade)

Each layer slots in additively behind clean traits (`Engine` / `GrammarChecker`, composed by
`WithGrammar`); a new layer never overrides the ones below it.

| Layer | What | Status |
|---|---|---|
| L0 | Segmentation (wtpsplit/SaT ONNX) | planned |
| **L1** | **Spelling (`is_known` + Norvig edit-1, via the engine lexicon)** | **done** |
| **L2** | **LT rule grammar — nlprule baseline + IR matcher over converted v6.7 rules** | **done** |
| **L3** | **Confusion-pair real-word errors (bigram + POS likelihood ratio over pruned n-grams)** | **done** |
| **L4** | **Neural GECToR edit-tagger (int8 ONNX via pure-Rust `rten`), iterative, `Source::Neural`** | **done (native + wasm; F0.5 0.545)** |

## Workspace

| Crate | Role |
|---|---|
| `rlt-ir` | Sealed IR for LT constructs (`Opaque` tail = computed coverage metric) |
| `rlt-convert` | Offline LT XML → IR → rkyv artifact converter (the heart) |
| `rlt-engine` | Vendored nlprule analysis engine behind `rlt-core::Engine` |
| `rlt-core` | Runtime: `Engine`/`GrammarChecker` seams, `Diagnostic` types, L1–L4 cascade composition |
| `rlt-tagger` | L4 neural edit-tagger: GECToR decoder + `rten` int8 inference + RoBERTa tokenizer |
| `rlt-cli` | `rlt check <file> [--matcher nlprule\|ir]` / `rlt convert` / `rlt tokens` |
| `rlt-wasm` | `wasm-bindgen` surface for the browser/Node |
| `pipeline/` | Offline L4 model pipeline (uv + Python): export + int8-quantize the GECToR tagger |
| `xtask` | `fetch-lt` / `gen-schema` / `fetch-engine` / `build-blob` / `build-l4` / `build-wasm` / `run-oracle` |

## Quick start

```sh
cargo xtask fetch-engine    # nlprule tokenizer + rules binaries (resumable)
cargo xtask fetch-lt        # pinned LT English resources + schemas (for the IR matcher)
cargo xtask build-blob      # compile LT v6.7 grammar -> resources/en.rkyv
cargo run -p rlt-cli -- check path/to/file.txt                # nlprule backend (default)
cargo run -p rlt-cli -- check --matcher ir path/to/file.txt   # our v6.7 IR matcher
cargo xtask build-l4        # export + int8-quantize the L4 neural tagger (uv + Python 3.12)
cargo xtask build-wasm      # wasm-pack build + Node smoke test
cargo xtask run-oracle      # score both backends against LT's example corpus
```

L4 is automatic when `resources/l4/` is present (after `build-l4`); otherwise the CLI runs L1–L3.

## Licensing

Code is `Apache-2.0 OR MIT`. LanguageTool-derived data artifacts (`en.rkyv`, `confusion.rkyv`) are
`LGPL-2.1` and not committed. The optional **L4 model** is derived from a non-commercial GECToR
checkpoint, so **any distribution that bundles L4 is non-commercial** (PolyForm Noncommercial); the
Rust code stays permissive and L4 is opt-in/separable (without `resources/l4/`, the checker is
L1–L3 only). See [`LICENSES.md`](LICENSES.md) and [`NOTICE`](NOTICE).
