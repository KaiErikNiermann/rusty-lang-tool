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

Pre-MVP. Building the deterministic core: **L1 spelling + L2 LanguageTool rule grammar**, English
only, as a Rust crate + CLI with a working `wasm32` build. See
`crates/` and the milestone breakdown in the design plan.

## Architecture (the cascade)

The MVP ships L1 + L2; later layers slot in additively behind clean traits.

| Layer | What | Status |
|---|---|---|
| L0 | Segmentation (wtpsplit/SaT ONNX) | planned |
| **L1** | **Spelling (morfologik FSA)** | **MVP** |
| **L2** | **LT rule grammar (XSD → IR → rkyv, vendored nlprule engine)** | **MVP** |
| L3 | Pruned/quantized confusion n-grams | planned |
| L4 | Quantized GECToR edit-tagger (ONNX/ORT-Web int8) | planned |

## Workspace

| Crate | Role |
|---|---|
| `rlt-ir` | Sealed IR for LT constructs (`Opaque` tail = computed coverage metric) |
| `rlt-convert` | Offline LT XML → IR → rkyv artifact converter (the heart) |
| `rlt-engine` | Vendored nlprule analysis engine behind `rlt-core::Engine` |
| `rlt-core` | Runtime: `Engine` seam, `Diagnostic` types, L1+L2 cascade |
| `rlt-cli` | `rlt check <file>` / `rlt convert` |
| `rlt-wasm` | `wasm-bindgen` surface for the browser/Node |
| `xtask` | `fetch-lt` / `build-blob` / `build-wasm` / `run-oracle` |

## Quick start

```sh
cargo xtask fetch-lt        # pull pinned LT English resources + schemas (resumable)
cargo xtask build-blob      # compile -> resources/en.rkyv
cargo run -p rlt-cli -- check path/to/file.txt
```

## Licensing

Code is `Apache-2.0 OR MIT`. LanguageTool-derived data artifacts are `LGPL-2.1` and not committed.
See [`LICENSES.md`](LICENSES.md).
