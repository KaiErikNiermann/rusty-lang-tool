"""Shared constants: the source model and the artifact-tuple file layout the Rust runtime loads."""

import os
from pathlib import Path

# The borrowed GECToR checkpoint (non-commercial license — see the repo's LICENSES/NOTICE).
# Swappable via $RLT_MODEL_ID — the Rust runtime is backbone-agnostic (it loads whatever tokenizer
# /labels/meta the export emits), so any gotutiyan GECToR checkpoint is a drop-in, e.g.
# `gotutiyan/gector-bert-base-cased-5k` (~13% smaller: bert's 29k vocab vs roberta's 50k embedding).
# A genuinely small web model (sub-50 MB) needs distillation — see pipeline/README.md.
MODEL_ID = os.environ.get("RLT_MODEL_ID", "gotutiyan/gector-roberta-base-5k")
# Grammarly's verb-form transform dictionary, needed to apply $TRANSFORM_VERB_* tags.
VERB_DICT_URL = "https://github.com/grammarly/gector/raw/master/data/verb-form-vocab.txt"

# Artifact tuple consumed by `crates/rlt-tagger`. `resources/l4/` is gitignored. The output dir is
# overridable via $RLT_L4_DIR (build a variant backbone to a scratch dir without clobbering).
REPO_ROOT = Path(__file__).resolve().parents[2]
OUT_DIR = Path(os.environ.get("RLT_L4_DIR", str(REPO_ROOT / "resources" / "l4")))

ONNX_FP32 = OUT_DIR / "gector.fp32.onnx"
ONNX_INT8 = OUT_DIR / "model.int8.onnx"
TOKENIZER_JSON = OUT_DIR / "tokenizer.json"
LABELS_JSON = OUT_DIR / "labels.json"
VERB_DICT = OUT_DIR / "verb-form-vocab.txt"
META_JSON = OUT_DIR / "meta.json"
METRICS_JSON = OUT_DIR / "metrics.json"

# Evaluation: the BEA-2019 W&I+LOCNESS dev set (gold M2), the standard ERRANT benchmark.
EVAL_DIR = OUT_DIR / "eval"
DEV_M2 = EVAL_DIR / "dev.gold.m2"
BEA_URL = "https://www.cl.cam.ac.uk/research/nl/bea2019st/data/wi+locness_v2.1.bea19.tar.gz"
