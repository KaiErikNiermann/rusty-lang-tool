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
absorbs format changes and the example oracle reports exactly which rules drifted.

## Licensing

LanguageTool rule data is **LGPL-2.1**. Any artifact derived from it (`en.rkyv`, compiled
dictionaries) inherits that license. See `../LICENSES.md` for the code-vs-data split.
