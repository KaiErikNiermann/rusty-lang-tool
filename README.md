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

- **nlprule baseline** (LT v5.2 rules): reproduces **52.8%**.
- **IR matcher** (our converter's LT **v6.7** rules): reproduces **55.3%** — the on-thesis path,
  already ahead of the baseline, with antipatterns/`<and>`/`<or>`/`<unify>` still to add.

## Architecture (the cascade)

The MVP ships L1 + L2; later layers slot in additively behind clean traits.

| Layer | What | Status |
|---|---|---|
| L0 | Segmentation (wtpsplit/SaT ONNX) | planned |
| **L1** | **Spelling (`is_known` + Norvig edit-1, via the engine lexicon)** | **done** |
| **L2** | **LT rule grammar — nlprule baseline + IR matcher over converted v6.7 rules** | **done** |
| L3 | Pruned/quantized confusion n-grams | planned |
| L4 | Quantized GECToR edit-tagger (ONNX/ORT-Web int8) | planned |

## Workspace

| Crate | Role |
|---|---|
| `rlt-ir` | Sealed IR for LT constructs (`Opaque` tail = computed coverage metric) |
| `rlt-convert` | Offline LT XML → IR → rkyv artifact converter (the heart) |
| `rlt-engine` | Vendored nlprule analysis engine behind `rlt-core::Engine` |
| `rlt-core` | Runtime: `Engine` seam, `Diagnostic` types, L1+L2 cascade |
| `rlt-cli` | `rlt check <file> [--matcher nlprule\|ir]` / `rlt convert` / `rlt tokens` |
| `rlt-wasm` | `wasm-bindgen` surface for the browser/Node |
| `xtask` | `fetch-lt` / `gen-schema` / `fetch-engine` / `build-blob` / `build-wasm` / `run-oracle` |

## Quick start

```sh
cargo xtask fetch-engine    # nlprule tokenizer + rules binaries (resumable)
cargo xtask fetch-lt        # pinned LT English resources + schemas (for the IR matcher)
cargo xtask build-blob      # compile LT v6.7 grammar -> resources/en.rkyv
cargo run -p rlt-cli -- check path/to/file.txt                # nlprule backend (default)
cargo run -p rlt-cli -- check --matcher ir path/to/file.txt   # our v6.7 IR matcher
cargo xtask build-wasm      # wasm-pack build + Node smoke test
cargo xtask run-oracle      # score both backends against LT's example corpus
```

## Licensing

Code is `Apache-2.0 OR MIT`. LanguageTool-derived data artifacts are `LGPL-2.1` and not committed.
See [`LICENSES.md`](LICENSES.md).
