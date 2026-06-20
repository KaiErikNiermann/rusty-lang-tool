// Stage the web demo's language artifacts + integrity manifest into `static/` so a plain
// `pnpm run dev` / `pnpm run build` works without the manual copy steps the README used to require.
//
// Runs `cargo xtask web-manifest` (which compresses each built `.rkyv` to both `.gz` (Reliable track)
// and `.br` (Fast track) and emits `web-artifacts.json`), writing the assets straight into
// `static/artifacts/` (where ARTIFACT_BASE_URL serves them in dev) and relocating the manifest to
// `static/web-artifacts.json` (the MANIFEST_URL root of trust).
//
// Language selection (so dev stays fast — gzipping all 7 at best compression takes minutes):
//   - default: `en` only (the demo's default language)
//   - `node scripts/stage-artifacts.mjs all`        -> every built language (used by `prebuild`)
//   - `node scripts/stage-artifacts.mjs en,de,fr`   -> an explicit subset
//   - env `RLT_WEB_LANGS` is the fallback when no argv is given
//
// CI safety: the Pages deploy downloads `web-artifacts.json` from the artifact Release and never builds
// the local `.rkyv` set, so when those source artifacts are absent we SKIP staging and keep whatever
// manifest is already present (the Release one). We only hard-fail when there is neither.
//
// The xtask is incremental (it reuses an up-to-date `.gz`), so re-running on an unchanged tree is
// near-instant — but a changed artifact is re-gzipped, keeping the staged bytes + hashes in sync.

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, renameSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const webDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = resolve(webDir, "..");
const artifactsDir = join(webDir, "static", "artifacts");
const manifestPath = join(webDir, "static", "web-artifacts.json");

const langs = (process.argv[2] ?? process.env.RLT_WEB_LANGS ?? "en").trim();
const langArgs = langs === "all" || langs === "" ? [] : ["--langs", langs];

// `resources/segment.srx` is the shared input every language build produces; its presence is the proxy
// for "the artifacts are built locally and we can (re)stage them".
if (!existsSync(join(repoRoot, "resources", "segment.srx"))) {
  if (existsSync(manifestPath)) {
    console.log("[stage-artifacts] no local artifacts; keeping the existing manifest (CI / Release flow)");
    process.exit(0);
  }
  console.error(
    "[stage-artifacts] no local artifacts and no manifest to fall back on.\n" +
      "  Build them first:\n" +
      "    cargo xtask fetch-lt\n" +
      "    for c in $(cargo run -q -p xtask -- lang-codes); do cargo run -p xtask -- build-lang --lang $c; done\n",
  );
  process.exit(1);
}

mkdirSync(artifactsDir, { recursive: true });

console.log(`[stage-artifacts] web-manifest (${langs}) -> static/artifacts/`);
const run = spawnSync(
  "cargo",
  ["run", "-q", "-p", "xtask", "--", "web-manifest", "--out", artifactsDir, ...langArgs],
  { cwd: repoRoot, stdio: "inherit" },
);

if (run.status !== 0) {
  console.error("\n[stage-artifacts] FAILED to build the manifest (see xtask output above).");
  process.exit(run.status ?? 1);
}

// The app loads the manifest from `${base}/web-artifacts.json` (static root), not from /artifacts.
renameSync(join(artifactsDir, "web-artifacts.json"), manifestPath);
console.log("[stage-artifacts] wrote static/web-artifacts.json");
