import adapter from "@sveltejs/adapter-static";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

// GitHub Pages serves a project site under /<repo>/. The deploy workflow passes BASE_PATH; locally
// it's empty (root). All asset/fetch URLs route through `base` from "$app/paths".
/** @type {"" | `/${string}`} */
const base = process.env.BASE_PATH ? `/${process.env.BASE_PATH.replace(/^\/+/, "")}` : "";

/** @type {import('@sveltejs/kit').Config} */
export default {
  preprocess: vitePreprocess(),
  kit: {
    // Fully static (no server). 404 fallback so the SPA route resolves under any path.
    adapter: adapter({ fallback: "404.html" }),
    paths: { base },
    // `$wasm` → the wasm-pack (--target web) bundle. kit.alias feeds both Vite and the generated
    // tsconfig paths, so TS resolves the .js import to its sibling rlt_wasm.d.ts.
    alias: { $wasm: "../crates/rlt-wasm/pkg/rlt_wasm.js" },
    // The whole app is client-only (WASM + Monaco are browser-only); see +layout.ts.
    prerender: { entries: ["*"] },
  },
};
