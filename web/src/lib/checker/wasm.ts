import type * as RltWasm from "$wasm";
// The `.wasm` binary as a Vite-served asset URL. The wasm-pack glue otherwise resolves it via
// `new URL("rlt_wasm_bg.wasm", import.meta.url)`, which fails in the bundled worker because the pkg
// lives outside the web root (Vite doesn't serve that sibling). Importing it `?url` makes Vite track
// the asset — serving it in dev and emitting a hashed copy on build — so we pass the URL to init().
import wasmUrl from "../../../../crates/rlt-wasm/pkg/rlt_wasm_bg.wasm?url";

let initPromise: Promise<typeof RltWasm> | null = null;

/**
 * Load + initialize the wasm module exactly once. The `--target web` bundle's default export is its
 * `init()`; it must be awaited before any `RltChecker` is constructed. The dynamic `import()` keeps the
 * wasm out of the SSR/prerender graph (browser-only).
 */
export function initWasm(): Promise<typeof RltWasm> {
  initPromise ??= (async () => {
    const mod = await import("$wasm");
    await mod.default({ module_or_path: wasmUrl });
    return mod;
  })();
  return initPromise;
}
