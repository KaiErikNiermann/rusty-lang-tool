# rlt web demo

A fully client-side grammar + spell checker: pick a language, the page fetches that language's compiled
artifacts once (verified + cached locally), and the WASM engine checks your text in a Monaco editor with
inline squiggles and quick-fixes. No login, no server, no telemetry.

## Architecture

- **WASM checker** — `RltChecker.with_native(lang, srx, tagger, disambig, grammar)` (L1 spelling + L2
  grammar) or `with_native_confusion(…, confusion)` (adds **L3 real-word errors** — their/there, to/too)
  from `crates/rlt-wasm`, built `--target web --no-default-features` (pure-native, no nlprule). en/de/ru/
  fr/es ship a confusion model; ar/it run L1+L2.
- **Web Worker** (`src/lib/checker/checker.worker.ts`) — wasm init, the artifact fetch/verify/decompress,
  the heavy rkyv deserialize, and every `check()` run **off the main thread**, so the UI never freezes on a
  100 MB-language load or a large-input check. The main thread (`worker-client.ts`) just sends text and
  renders the diagnostics that come back.
- **ArtifactStore** (`src/lib/artifacts/store.ts`) — streams each gzipped artifact, verifies its SHA-256
  against the manifest, decompresses via the native `DecompressionStream`, caches it in the Cache Storage
  API keyed by content hash, and re-fetches with backoff on any corruption. The cache key is the hash, so
  bumping an artifact transparently invalidates it; a full storage quota degrades to no-cache (re-download
  next time) rather than failing.
- **Root of trust** — `web-artifacts.json` (emitted by `cargo xtask web-manifest`) records the SHA-256 +
  sizes of every compressed artifact. It's baked into the deployed site; the artifacts live on a GitHub
  Release. The same CI job emits both, so the hashes always match the bytes.
- **spanmap** (`src/lib/checker/spanmap.ts`) — maps the engine's UTF-8 **byte** spans to Monaco's UTF-16
  positions (the correctness crux; unit-tested in `spanmap.test.ts`).

## Run it locally

The language `.rkyv` artifacts must be built once (they're large + gitignored):

```bash
cargo xtask fetch-lt
for c in $(cargo run -q -p xtask -- lang-codes); do cargo run -p xtask -- build-lang --lang $c; done
```

Then just run the dev server — `predev` wasm-packs the checker **and** stages the artifacts + integrity
manifest into `static/` automatically (no manual copying):

```bash
cd web
pnpm install
pnpm run dev          # http://localhost:5173
```

By default dev stages only **English** (gzipping all 7 languages at best compression takes minutes). To
test other languages, stage them explicitly, then reload:

```bash
pnpm run stage:all                       # all built languages
RLT_WEB_LANGS=en,de,fr pnpm run stage    # or an explicit subset
```

Staging is incremental (an unchanged artifact is not re-gzipped) and idempotent, so re-running is cheap.
For production the artifacts are served from a GitHub Release; set `VITE_ARTIFACT_BASE_URL` to the
release download URL and `BASE_PATH` to the Pages sub-path (the CI workflows do this). When no local
`.rkyv` artifacts are present (e.g. the Pages build, which downloads `web-artifacts.json` from the
Release), staging is skipped and the existing manifest is kept.

> The gzipped artifacts are gzip *content* the app decompresses in JS, so they must reach the browser
> verbatim. A dev/preview Vite middleware (`rawArtifacts` in `vite.config.ts`) serves `/artifacts/*.gz`
> as raw `application/octet-stream` — otherwise the dev server would set `Content-Encoding: gzip`, the
> browser would transparently inflate them, and the SHA-256 would no longer match the manifest.

## Scripts

| command | what |
| --- | --- |
| `pnpm run dev` | dev server (wasm-packs the checker + stages English first) |
| `pnpm run build` | wasm-pack + stage all languages + static build into `build/` |
| `pnpm run stage` / `stage:all` | (re)stage artifacts + manifest into `static/` (English / all) |
| `pnpm run check` | `svelte-check` (strict TS) |
| `pnpm test` | vitest (spanmap vectors) |

## CI

- `.github/workflows/release-artifacts.yml` — builds every language, gzips + hashes them, publishes the
  `.gz` assets + `web-artifacts.json` to a `artifacts-<ltversion>` Release.
- `.github/workflows/deploy-pages.yml` — bakes that manifest into the site, wasm-packs the checker, builds,
  and deploys to GitHub Pages.
