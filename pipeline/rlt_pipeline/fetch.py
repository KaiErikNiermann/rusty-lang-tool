"""Fetch pipeline inputs that aren't part of the HF checkpoint — currently Grammarly's verb-form
transform dictionary, needed to apply `$TRANSFORM_VERB_*` tags. Idempotent (skips if present)."""

from __future__ import annotations

import tarfile
import tempfile
import urllib.request

from .paths import BEA_URL, DEV_M2, EVAL_DIR, OUT_DIR, VERB_DICT, VERB_DICT_URL

# The dev gold M2 inside the BEA-2019 tarball.
_BEA_DEV_MEMBER = "wi+locness/m2/ABCN.dev.gold.bea19.m2"


def _fetch_verb_dict() -> None:
    if VERB_DICT.exists():
        print(f"{VERB_DICT} exists — skipping")
        return
    urllib.request.urlretrieve(VERB_DICT_URL, VERB_DICT)  # noqa: S310 (trusted github URL)
    print(f"wrote {VERB_DICT} ({VERB_DICT.stat().st_size} bytes)")


def _fetch_bea_dev() -> None:
    if DEV_M2.exists():
        print(f"{DEV_M2} exists — skipping")
        return
    EVAL_DIR.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(suffix=".tar.gz") as tmp:
        urllib.request.urlretrieve(BEA_URL, tmp.name)  # noqa: S310 (trusted cl.cam.ac.uk URL)
        with tarfile.open(tmp.name) as tar:
            src = tar.extractfile(_BEA_DEV_MEMBER)
            if src is None:
                raise RuntimeError(f"{_BEA_DEV_MEMBER} not found in BEA tarball")
            DEV_M2.write_bytes(src.read())
    print(f"wrote {DEV_M2}")


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    _fetch_verb_dict()
    _fetch_bea_dev()


if __name__ == "__main__":
    main()
