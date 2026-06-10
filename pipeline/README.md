# L4 offline pipeline (`rlt-pipeline`)

Produces the **L4 neural edit-tagger artifact tuple** the Rust runtime (`crates/rlt-tagger`) loads.
Offline-only (no runtime sidecar): it exports the GECToR checkpoint to a quantized ONNX graph that
`rten` runs natively and in wasm. The graph is a dumb `(input_ids, attention_mask) → (logits_labels,
logits_d)` tensor function; word-pooling and edit decoding live in Rust.

Uses **uv** (not Poetry) and **Python 3.12** — torch has no 3.14 wheels yet (the system default), and
uv resolves the CPU-torch index + the `gector` git source far more reliably than Poetry.

## Run

```sh
cd pipeline
uv sync                              # torch (CPU) + transformers<5 + gector + onnx(+runtime)
uv run python -m rlt_pipeline.export     # checkpoint -> resources/l4/gector.fp32.onnx + tokenizer/labels/meta
uv run python -m rlt_pipeline.quantize   # -> resources/l4/model.int8.onnx (~129 MB, from ~512 MB)
uv run python -m rlt_pipeline.fetch      # verb-form-vocab.txt + BEA-2019 dev (for the eval)
uv run python -m rlt_pipeline.evaluate   # ERRANT F0.5 vs BEA dev -> resources/l4/metrics.json
uv run ruff check rlt_pipeline && uv run pyright rlt_pipeline
```

`evaluate` runs the **int8 ONNX** through onnxruntime with gector's faithful decode (reusing its
`edit_src_by_tags` + verb dict + word-pooling), scores with ERRANT, and sweeps
`(keep_confidence, min_error_prob)` to calibrate the threshold *after* quantization. `--limit N`
caps the dev sentences for a quick run. The calibrated thresholds become `rlt-tagger`'s
`TaggerConfig` defaults.

`rlt_pipeline.inspect_model` dumps the checkpoint's config/labels/tokenizer specials (used to derive
`meta.json`).

## Artifact tuple → `resources/l4/` (gitignored)

| File | Role |
|---|---|
| `model.int8.onnx` | int8-dynamic-quantized GECToR graph (`rten` loads ONNX directly) |
| `tokenizer.json` | RoBERTa BPE fast tokenizer (the `tokenizers` Rust crate reads it) |
| `labels.json` | 5002-entry tag vocab; label-logit index maps 1:1 to a label id |
| `verb-form-vocab.txt` | Grammarly verb-form dict for `$TRANSFORM_VERB_*` |
| `meta.json` | indices the Rust decoder agrees on: `keep_label_index=1`, `detect_incorrect_index=1`, head dims, `$START` token, max subwords |

## Model & licensing

Source model: **`gotutiyan/gector-roberta-base-5k`** — **non-commercial**. Any distribution including
the L4 artifact is therefore non-commercial (see the repo `LICENSES.md` / `NOTICE`). The Rust *code*
stays Apache-2.0/MIT.

## Status

Export + int8 quantize + ERRANT eval done. The int8 graph runs in `rten` and produces correct edits
post-quantization. On **BEA-2019 W&I+LOCNESS dev (4384 sentences)** the int8 model scores
**F0.5 = 0.545** (P = 0.653, R = 0.328) at the calibrated `keep_confidence=0.2, min_error_prob=0.66`
(which become `rlt-tagger`'s defaults). `metrics.json` is the promotion gate (`evaluate --floor`).
