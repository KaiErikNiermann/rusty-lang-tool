import { sveltekit } from "@sveltejs/kit/vite";
import { defineConfig } from "vite";

// `$wasm` (the wasm-pack --target web bundle) is aliased via kit.alias in svelte.config.js, which feeds
// both Vite and TS. It's imported dynamically (client-only) so it never enters the SSR/prerender graph,
// and excluded from dep-optimization so Vite serves the .wasm with the right MIME type.
export default defineConfig({
  plugins: [sveltekit()],
  optimizeDeps: { exclude: ["$wasm"] },
  // The checker worker dynamically imports `$wasm` (code-splitting), so it must be bundled as an ES
  // module — Vite's default IIFE worker format can't code-split.
  worker: { format: "es" },
  // Vitest: jsdom for the spanmap/DOM-touching unit tests.
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts"],
  },
});
