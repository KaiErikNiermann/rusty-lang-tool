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
uv run python -m rlt_pipeline.fetch      # verb-form-vocab.txt (for $TRANSFORM_VERB_* tags)
uv run ruff check rlt_pipeline && uv run pyright rlt_pipeline
```

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

Export + int8 quantize done; the int8 graph is verified to run in `rten` and to produce correct edits
post-quantization (e.g. "She go" → `$TRANSFORM_VERB_VB_VBZ`, "a apple" → `$REPLACE_an`). The ERRANT
F0.5 evaluation (`metrics.json`, the promotion gate) lands with M7.7.
