"""Export the GECToR checkpoint to a clean ``(input_ids, attention_mask) -> (logits_labels,
logits_d)`` ONNX graph (per-subword logits), plus the tokenizer / label vocab / meta the Rust
decoder needs. Word-pooling (first-subword-per-word) and edit decoding live in Rust, not the graph.
"""

from __future__ import annotations

import json
from typing import Any, cast

import torch
from torch import Tensor, nn
from transformers import AutoTokenizer

from gector import GECToR

from .paths import (
    LABELS_JSON,
    META_JSON,
    MODEL_ID,
    ONNX_FP32,
    OUT_DIR,
    TOKENIZER_JSON,
)


class ExportWrapper(nn.Module):
    """Reduce GECToR's dataclass output to the two logit tensors ONNX should expose."""

    def __init__(self, model: nn.Module) -> None:
        super().__init__()
        self.model = model

    def forward(self, input_ids: Tensor, attention_mask: Tensor) -> tuple[Tensor, Tensor]:
        out: Any = self.model(input_ids=input_ids, attention_mask=attention_mask)
        return out.logits_labels, out.logits_d


def _labels_in_index_order(label2id: dict[str, int]) -> list[str]:
    by_id = {i: tag for tag, i in label2id.items()}
    return [by_id[i] for i in range(len(by_id))]


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)

    model: Any = GECToR.from_pretrained(MODEL_ID)
    model.eval()
    tokenizer: Any = AutoTokenizer.from_pretrained(MODEL_ID)
    cfg: Any = model.config

    labels = _labels_in_index_order(cast("dict[str, int]", cfg.label2id))
    LABELS_JSON.write_text(json.dumps(labels, ensure_ascii=False), encoding="utf-8")
    tokenizer.save_pretrained(OUT_DIR)  # writes tokenizer.json (fast) among others
    assert TOKENIZER_JSON.exists(), "fast tokenizer.json not written"

    # Everything the Rust decoder must agree on, so the graph stays a dumb tensor->tensor function.
    meta = {
        "model_id": MODEL_ID,
        "num_labels": int(cfg.num_labels),
        # Heads emit num_labels-1 / d_num_labels-1 logits — the <PAD> class (last) is excluded — so
        # the label-logit index maps 1:1 to a label id, and `labels.json[argmax]` is correct.
        "label_head_dim": int(cfg.num_labels) - 1,
        "keep_label_index": labels.index("$KEEP"),
        "oov_label_index": labels.index("<OOV>") if "<OOV>" in labels else -1,
        # detect head emits 2 logits: [$CORRECT, $INCORRECT] -> incorrect is index 1.
        "detect_head_dim": int(cfg.d_num_labels) - 1,
        "detect_incorrect_index": int(cfg.d_num_labels) - 2,
        "start_token": "$START",
        "max_subwords": 256,
    }
    META_JSON.write_text(json.dumps(meta, indent=2), encoding="utf-8")

    # Dynamic dummy: a 4-word sentence ($START prepended) -> traceable batch=1 graph.
    enc = tokenizer(
        ["$START", "I", "beleive", "it"],
        is_split_into_words=True,
        return_tensors="pt",
    )
    wrapper = ExportWrapper(model)
    with torch.no_grad():
        torch.onnx.export(
            wrapper,
            (enc["input_ids"], enc["attention_mask"]),
            str(ONNX_FP32),
            input_names=["input_ids", "attention_mask"],
            output_names=["logits_labels", "logits_d"],
            dynamic_axes={
                "input_ids": {0: "batch", 1: "seq"},
                "attention_mask": {0: "batch", 1: "seq"},
                "logits_labels": {0: "batch", 1: "seq"},
                "logits_d": {0: "batch", 1: "seq"},
            },
            opset_version=17,
            dynamo=False,
        )
    print(f"wrote {ONNX_FP32} ({ONNX_FP32.stat().st_size / 1e6:.1f} MB)")
    print(f"labels={len(labels)} keep_index={meta['keep_label_index']} meta={META_JSON}")


if __name__ == "__main__":
    main()
