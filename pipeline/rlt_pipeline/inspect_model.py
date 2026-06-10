"""Dump the GECToR checkpoint's config, label vocab, and tokenizer specials — run once to learn
the exact attribute names before finalizing the export (gector stores labels/$START its own way)."""

from __future__ import annotations

from typing import Any

from transformers import AutoTokenizer

from gector import GECToR

from .paths import MODEL_ID


def main() -> None:
    model: Any = GECToR.from_pretrained(MODEL_ID)
    tok: Any = AutoTokenizer.from_pretrained(MODEL_ID)
    cfg: Any = model.config

    print("=== config keys ===")
    for k in sorted(vars(cfg)):
        v = getattr(cfg, k)
        if k in {"id2label", "label2id"}:
            print(f"{k}: <{len(v)} entries> sample={list(v.items())[:3]}")
        else:
            print(f"{k}: {v!r}")

    print("\n=== label vocab probes ===")
    for attr in ("num_labels", "d_num_labels", "label_pad_token", "keep_label"):
        print(f"cfg.{attr}:", getattr(cfg, attr, "<missing>"))

    print("\n=== tokenizer ===")
    print("model_max_length:", tok.model_max_length)
    print("special tokens:", tok.special_tokens_map)
    print("added tokens:", tok.get_added_vocab())
    print("is_fast:", tok.is_fast)
    enc = tok(["$START", "I", "beleive", "it"], is_split_into_words=True)
    print("sample input_ids:", enc["input_ids"])
    print("sample word_ids:", enc.word_ids())


if __name__ == "__main__":
    main()
