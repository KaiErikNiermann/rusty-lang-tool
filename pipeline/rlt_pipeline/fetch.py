"""Fetch pipeline inputs that aren't part of the HF checkpoint — currently Grammarly's verb-form
transform dictionary, needed to apply `$TRANSFORM_VERB_*` tags. Idempotent (skips if present)."""

from __future__ import annotations

import urllib.request

from .paths import OUT_DIR, VERB_DICT, VERB_DICT_URL


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    if VERB_DICT.exists():
        print(f"{VERB_DICT} exists — skipping")
        return
    urllib.request.urlretrieve(VERB_DICT_URL, VERB_DICT)  # noqa: S310 (trusted github URL)
    print(f"wrote {VERB_DICT} ({VERB_DICT.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
