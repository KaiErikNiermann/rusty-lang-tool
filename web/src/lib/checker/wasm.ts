import type * as RltWasm from "$wasm";

let initPromise: Promise<typeof RltWasm> | null = null;

/**
 * Load + initialize the wasm module exactly once. The `--target web` bundle's default export is its
 * `init()`; it must be awaited before any `RltChecker` is constructed. The dynamic `import()` keeps the
 * wasm out of the SSR/prerender graph (browser-only).
 */
export function initWasm(): Promise<typeof RltWasm> {
  initPromise ??= (async () => {
    const mod = await import("$wasm");
    await mod.default();
    return mod;
  })();
  return initPromise;
}
