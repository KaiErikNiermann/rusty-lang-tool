# `resources/`

LanguageTool's rule data is **not committed** here — it is fetched on demand and gitignored
(`lt/`, `*.rkyv`, `*.dict`). The full LT tree is ~274 MB across ~40 languages; we pull only the
English subset + the rule XSD schemas.

## Fetch (resumable)

```sh
cargo xtask fetch-lt     # sparse checkout of en/ + schemas at the pinned LT tag
cargo xtask build-blob   # compile grammar.xml + dicts -> resources/en.rkyv
```

The pinned LanguageTool version lives in `xtask/src/main.rs` (`LT_VERSION`). Bumping it and
re-running is the entire "track a new LT release" workflow — the converter's schema codegen
absorbs format changes and the example oracle reports exactly which rules drifted. Any task can
target a different release for one invocation via `RLT_LT_VERSION=v6.6 cargo xtask fetch-lt`.

## Adaptability gauge

`cargo xtask adapt-sweep [--from v5.4 --to v6.8]` runs the whole pipeline (fetch → gen-schema →
build-blob → score-oracle) across past LT releases and writes `../docs/adaptability.md` — a live
gauge of how cleanly the self-maintaining converter absorbs schema/rule changes (a proxy for
future releases). It is **resumable** (per-version results in `adaptability/results.json`, skipped
on re-run unless `--force`) and **restores** the pinned `LT_VERSION` working tree when done. The
load-bearing signal is whether `rlt-convert` still *compiles* against each version's regenerated
bindings. `cargo run -p rlt-cli -- score-oracle [--json]` scores one version standalone.

## Licensing

LanguageTool rule data is **LGPL-2.1**. Any artifact derived from it (`en.rkyv`, compiled
dictionaries) inherits that license. See `../LICENSES.md` for the code-vs-data split.
