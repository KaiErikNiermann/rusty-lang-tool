# rlt web demo

A fully client-side grammar + spell checker: pick a language, the page fetches that language's compiled
artifacts once (verified + cached locally), and the WASM engine checks your text in a Monaco editor with
inline squiggles and quick-fixes. No login, no server, no telemetry.

## Architecture

- **WASM checker** — `RltChecker.with_native(lang, srx, tagger, disambig, grammar)` from `crates/rlt-wasm`,
  built `--target web --no-default-features` (pure-native L1 spelling + L2 grammar, no nlprule).
- **ArtifactStore** (`src/lib/artifacts/store.ts`) — streams each gzipped artifact, verifies its SHA-256
  against the manifest, decompresses via the native `DecompressionStream`, caches it in the Cache Storage
  API keyed by content hash, and re-fetches with backoff on any corruption. The cache key is the hash, so
  bumping an artifact transparently invalidates it.
- **Root of trust** — `web-artifacts.json` (emitted by `cargo xtask web-manifest`) records the SHA-256 +
  sizes of every compressed artifact. It's baked into the deployed site; the artifacts live on a GitHub
  Release. The same CI job emits both, so the hashes always match the bytes.
- **spanmap** (`src/lib/checker/spanmap.ts`) — maps the engine's UTF-8 **byte** spans to Monaco's UTF-16
  positions (the correctness crux; unit-tested in `spanmap.test.ts`).

## Run it locally

```bash
# 1. Build the language artifacts + the integrity manifest (needs the .rkyv built first:
#    `cargo xtask fetch-lt && for c in $(cargo run -q -p xtask -- lang-codes); do
#       cargo run -p xtask -- build-lang --lang $c; done`)
cargo run -p xtask -- web-manifest --out dist/web-artifacts

# 2. Point the app at those artifacts and run the dev server.
cd web
pnpm install
cp ../dist/web-artifacts/web-artifacts.json static/web-artifacts.json
mkdir -p static/artifacts && cp ../dist/web-artifacts/*.gz static/artifacts/
pnpm run dev          # http://localhost:5173  (ARTIFACT_BASE_URL defaults to /artifacts)
```

For production the artifacts are served from a GitHub Release; set `VITE_ARTIFACT_BASE_URL` to the
release download URL and `BASE_PATH` to the Pages sub-path (the CI workflows do this).

## Scripts

| command | what |
| --- | --- |
| `pnpm run dev` | dev server (rebuilds the wasm pkg first) |
| `pnpm run build` | wasm-pack + static build into `build/` |
| `pnpm run check` | `svelte-check` (strict TS) |
| `pnpm test` | vitest (spanmap vectors) |

## CI

- `.github/workflows/release-artifacts.yml` — builds every language, gzips + hashes them, publishes the
  `.gz` assets + `web-artifacts.json` to a `artifacts-<ltversion>` Release.
- `.github/workflows/deploy-pages.yml` — bakes that manifest into the site, wasm-packs the checker, builds,
  and deploys to GitHub Pages.
