"""ERRANT F0.5 evaluation of the **quantized** L4 model — the promotion gate (`metrics.json`).

Runs `model.int8.onnx` through onnxruntime with gector's faithful decode (reusing gector's own
`edit_src_by_tags` / verb dict / word-pooling, and replicating `GECToR.predict`'s thresholding), then
scores the corrected sentences against the BEA-2019 W&I+LOCNESS dev set with ERRANT.

Gold and hypothesis edits are both re-derived through one ERRANT annotator from `(source, corrected)`
so they share a tokenization (the official gold M2 was built the same way) — robust to spaCy version
drift. Sweeps a small grid of `(keep_confidence, min_error_prob)` to calibrate the threshold *after*
quantization, and records the best F0.5 plus the full sweep.
"""

from __future__ import annotations

import argparse
import json
from typing import Any, cast

import numpy as np
import onnxruntime as ort
from numpy.typing import NDArray
from transformers import AutoTokenizer

import errant
from gector.predict import edit_src_by_tags, get_word_masks_from_word_ids, load_verb_dict

from .paths import (
    DEV_M2,
    LABELS_JSON,
    META_JSON,
    METRICS_JSON,
    ONNX_INT8,
    OUT_DIR,
    VERB_DICT,
)

# Threshold grid (keep_confidence, min_error_prob) — biased toward the precision a writer wants.
GRID: list[tuple[float, float]] = [(0.0, 0.0), (0.0, 0.4), (0.2, 0.5), (0.2, 0.66)]


def _softmax(x: NDArray[np.float32]) -> NDArray[np.float32]:
    x = x - x.max(axis=-1, keepdims=True)
    e = np.exp(x)
    return cast("NDArray[np.float32]", e / e.sum(axis=-1, keepdims=True))


class OnnxGec:
    """The quantized GECToR tagger run via onnxruntime, decoded with gector's logic."""

    def __init__(self) -> None:
        self.sess = ort.InferenceSession(str(ONNX_INT8))
        self.tok: Any = AutoTokenizer.from_pretrained(str(OUT_DIR))
        labels: list[str] = json.loads(LABELS_JSON.read_text())
        self.id2label = dict(enumerate(labels))
        meta = json.loads(META_JSON.read_text())
        self.keep_index: int = meta["keep_label_index"]
        self.incor_index: int = meta["detect_incorrect_index"]
        self.max_len: int = meta.get("max_subwords", 256)
        self.no_correct_ids = {self.keep_index, labels.index("<OOV>"), labels.index("<PAD>")}
        self.encode, self.decode = load_verb_dict(str(VERB_DICT))

    def _predict(
        self, srcs: list[list[str]], keep_confidence: float, min_error_prob: float, batch_size: int
    ) -> tuple[list[list[str]], list[bool]]:
        pred_labels: list[list[str]] = []
        no_corrections: list[bool] = []
        for i in range(0, len(srcs), batch_size):
            batch = srcs[i : i + batch_size]
            enc = self.tok(
                batch,
                return_tensors="np",
                max_length=self.max_len,
                padding=True,
                truncation=True,
                is_split_into_words=True,
                add_special_tokens=True,
            )
            word_masks = np.array(get_word_masks_from_word_ids(enc.word_ids, len(batch)))
            outputs = self.sess.run(
                None,
                {
                    "input_ids": enc["input_ids"].astype(np.int64),
                    "attention_mask": enc["attention_mask"].astype(np.int64),
                },
            )
            prob_lab = _softmax(cast("NDArray[np.float32]", outputs[0]))
            prob_d = _softmax(cast("NDArray[np.float32]", outputs[1]))
            prob_lab[:, :, self.keep_index] += keep_confidence
            pred_ids = prob_lab.argmax(axis=-1)
            # Sentence-level detect gate, then token-level label-confidence gate (GECToR.predict).
            max_err = (prob_d[:, :, self.incor_index] * word_masks).max(axis=-1)
            pred_ids[max_err < min_error_prob, :] = self.keep_index
            pred_ids[prob_lab.max(axis=-1) < min_error_prob] = self.keep_index

            for b in range(len(batch)):
                word_ids = enc.word_ids(b)
                labels: list[str] = []
                no_correct = True
                previous = None
                for j, idx in enumerate(word_ids):
                    if idx is None:
                        continue
                    if idx != previous:
                        labels.append(self.id2label[int(pred_ids[b][j])])
                        if int(pred_ids[b][j]) not in self.no_correct_ids:
                            no_correct = False
                    previous = idx
                pred_labels.append(labels)
                no_corrections.append(no_correct)
        return pred_labels, no_corrections

    def correct(
        self,
        sources: list[str],
        keep_confidence: float,
        min_error_prob: float,
        n_iteration: int,
        batch_size: int = 64,
    ) -> list[str]:
        srcs = [["$START", *s.split(" ")] for s in sources]
        final: list[str] = ["-1"] * len(srcs)
        todo = srcs
        idx = list(range(len(srcs)))
        for _ in range(n_iteration):
            pred_labels, no_corrections = self._predict(
                todo, keep_confidence, min_error_prob, batch_size
            )
            nxt_src: list[list[str]] = []
            nxt_lab: list[list[str]] = []
            nxt_idx: list[int] = []
            for k, done in enumerate(no_corrections):
                if done:
                    final[idx[k]] = " ".join(todo[k]).replace("$START ", "")
                else:
                    nxt_src.append(todo[k])
                    nxt_lab.append(pred_labels[k])
                    nxt_idx.append(idx[k])
            if not nxt_src:
                break
            # gector annotates this `-> List[str]` but it returns one token list per sentence.
            todo = cast(
                "list[list[str]]", edit_src_by_tags(nxt_src, nxt_lab, self.encode, self.decode)
            )
            idx = nxt_idx
        for k in range(len(todo)):
            final[idx[k]] = " ".join(todo[k]).replace("$START ", "")
        return final


def read_m2(text: str) -> list[tuple[str, str]]:
    """Parse a gold M2 into `(source, gold_correction)` pairs (annotator 0, edits applied)."""
    out: list[tuple[str, str]] = []
    source: str | None = None
    edits: list[tuple[int, int, str]] = []

    def flush() -> None:
        if source is not None:
            out.append((source, _apply(source, edits)))

    for line in text.splitlines():
        if line.startswith("S "):
            flush()
            source = line[2:]
            edits = []
        elif line.startswith("A ") and source is not None:
            parts = line[2:].split("|||")
            if parts[1] == "noop" or parts[5].strip() != "0":
                continue
            span = parts[0].split()
            start, end = int(span[0]), int(span[1])
            if start < 0:
                continue
            edits.append((start, end, parts[2]))
    flush()
    return out


def _apply(source: str, edits: list[tuple[int, int, str]]) -> str:
    toks = source.split(" ")
    for start, end, corr in sorted(edits, key=lambda e: e[0], reverse=True):
        repl = [] if corr in {"", "-NONE-"} else corr.split(" ")
        toks[start:end] = repl
    return " ".join(toks)


def score(
    annotator: Any, sources: list[str], golds: list[str], hyps: list[str]
) -> dict[str, float | int]:
    """Edit-level TP/FP/FN and F0.5 — both gold and hyp edits derived through one annotator."""
    tp = fp = fn = 0
    for src, gold, hyp in zip(sources, golds, hyps, strict=True):
        orig = annotator.parse(src)
        gold_edits = {(e.o_start, e.o_end, e.c_str) for e in annotator.annotate(orig, annotator.parse(gold))}
        hyp_edits = {(e.o_start, e.o_end, e.c_str) for e in annotator.annotate(orig, annotator.parse(hyp))}
        tp += len(gold_edits & hyp_edits)
        fp += len(hyp_edits - gold_edits)
        fn += len(gold_edits - hyp_edits)
    p = tp / (tp + fp) if tp + fp else 0.0
    r = tp / (tp + fn) if tp + fn else 0.0
    f05 = (1.25 * p * r) / (0.25 * p + r) if (0.25 * p + r) else 0.0
    return {"tp": tp, "fp": fp, "fn": fn, "precision": p, "recall": r, "f0_5": f05}


def main() -> None:
    parser = argparse.ArgumentParser(description="ERRANT F0.5 eval of the int8 L4 model.")
    parser.add_argument("--limit", type=int, default=0, help="cap dev sentences (0 = all)")
    parser.add_argument("--iterations", type=int, default=5)
    parser.add_argument(
        "--floor", type=float, default=0.50, help="fail (nonzero exit) if best F0.5 < floor"
    )
    args = parser.parse_args()

    if not DEV_M2.exists():
        raise SystemExit(f"missing {DEV_M2} — run `python -m rlt_pipeline.fetch` first")
    pairs = read_m2(DEV_M2.read_text(encoding="utf-8"))
    if args.limit:
        pairs = pairs[: args.limit]
    sources = [s for s, _ in pairs]
    golds = [g for _, g in pairs]

    gec = OnnxGec()
    annotator = errant.load("en")
    print(f"evaluating {len(sources)} BEA-2019 dev sentences, {args.iterations} iterations")

    sweep: list[dict[str, Any]] = []
    for keep_confidence, min_error_prob in GRID:
        hyps = gec.correct(sources, keep_confidence, min_error_prob, args.iterations)
        m = score(annotator, sources, golds, hyps)
        m |= {"keep_confidence": keep_confidence, "min_error_prob": min_error_prob}
        sweep.append(m)
        print(
            f"  kc={keep_confidence} mep={min_error_prob}: "
            f"P={m['precision']:.3f} R={m['recall']:.3f} F0.5={m['f0_5']:.3f}"
        )

    best = max(sweep, key=lambda r: r["f0_5"])
    metrics = {
        "model": "model.int8.onnx",
        "dataset": "BEA-2019 W&I+LOCNESS dev",
        "n_sentences": len(sources),
        "iterations": args.iterations,
        "best": best,
        "sweep": sweep,
    }
    METRICS_JSON.write_text(json.dumps(metrics, indent=2), encoding="utf-8")
    print(f"best: F0.5={best['f0_5']:.3f} (kc={best['keep_confidence']}, mep={best['min_error_prob']})")
    print(f"wrote {METRICS_JSON}")
    if best["f0_5"] < args.floor:
        raise SystemExit(f"GATE FAILED: best F0.5 {best['f0_5']:.3f} < floor {args.floor}")


if __name__ == "__main__":
    main()
