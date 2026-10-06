import adapter from "@sveltejs/adapter-static";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

import { sveltekit } from "@sveltejs/kit/vite";
import type { Connect, Plugin } from "vite";
// `defineConfig` from vitest/config (a superset of vite's) so the `test` block typechecks — vitest 4
// no longer augments vite's own `UserConfig` with a `test` key.
import { defineConfig } from "vitest/config";

const ARTIFACTS_DIR = fileURLToPath(new URL("static/artifacts", import.meta.url));

// The language artifacts are compressed *content* we decompress in JS (gzip via DecompressionStream,
// brotli via our wasm) — they must reach the browser verbatim. But Vite's dev/preview static server
// treats a `.gz`/`.br` file as transport-compressed and sends `Content-Encoding: gzip`/`br`, so the
// browser transparently re-inflates it and the bytes the fetch sees no longer match the manifest's
// SHA-256 (→ "integrity mismatch"). This middleware serves `/artifacts/*.{gz,br}` as raw octet-stream
// with no Content-Encoding. Production is unaffected (the artifacts come from a GitHub Release, which
// already serves them verbatim).
function rawArtifacts(): Plugin {
  const handler: Connect.NextHandleFunction = (req, res, next) => {
    const path = (req.url ?? "").split("?", 1)[0] ?? "";
    const marker = "/artifacts/";
    const at = path.indexOf(marker);
    if (at === -1 || !(path.endsWith(".gz") || path.endsWith(".br"))) return next();
    const name = path.slice(at + marker.length);
    if (name.includes("/")) return next(); // flat dir only; no traversal
    readFile(`${ARTIFACTS_DIR}/${name}`).then(
      (buf) => {
        res.setHeader("Content-Type", "application/octet-stream");
        res.setHeader("Content-Length", buf.length);
        res.end(buf);
      },
      () => next(),
    );
  };
  return {
    name: "rlt-raw-artifacts",
    configureServer: (server) => void server.middlewares.use(handler),
    configurePreviewServer: (server) => void server.middlewares.use(handler),
  };
}

// GitHub Pages serves a project site under /<repo>/. The deploy workflow passes BASE_PATH; locally
// it's empty (root). All asset/fetch URLs route through `resolve` from "$app/paths".
const base: "" | `/${string}` = process.env.BASE_PATH
  ? `/${process.env.BASE_PATH.replace(/^\/+/, "")}`
  : "";

// `$wasm` (the wasm-pack --target web bundle) is aliased via the sveltekit() `alias` option, which feeds
// both Vite and TS. It's imported dynamically (client-only) so it never enters the SSR/prerender graph,
// and excluded from dep-optimization so Vite serves the .wasm with the right MIME type.
export default defineConfig({
  plugins: [
    rawArtifacts(),
    sveltekit({
      preprocess: vitePreprocess(),
      // Fully static (no server). 404 fallback so the SPA route resolves under any path.
      adapter: adapter({ fallback: "404.html" }),
      paths: { base },
      // `$wasm` → the wasm-pack (--target web) bundle. `alias` feeds both Vite and the generated
      // tsconfig paths, so TS resolves the .js import to its sibling rlt_wasm.d.ts.
      alias: { $wasm: "../crates/rlt-wasm/pkg/rlt_wasm.js" },
      // The whole app is client-only (WASM + Monaco are browser-only); see +layout.ts.
      prerender: { entries: ["*"] },
    }),
  ],
  optimizeDeps: { exclude: ["$wasm"] },
  // The wasm-pack bundle (`$wasm`) + its `rlt_wasm_bg.wasm` live in ../crates/rlt-wasm/pkg, outside the
  // web root, so the dev server must be allowed to serve from the repo root (the `?url` asset import in
  // checker/wasm.ts pulls the binary from there).
  server: { fs: { allow: [".."] } },
  // The checker worker dynamically imports `$wasm` (code-splitting), so it must be bundled as an ES
  // module — Vite's default IIFE worker format can't code-split.
  worker: { format: "es" },
  // Vitest: jsdom for the spanmap/DOM-touching unit tests.
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts"],
  },
});
