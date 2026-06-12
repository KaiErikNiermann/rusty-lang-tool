// The whole app is client-only: the WASM engine and Monaco are browser-only, and there's no server.
// `prerender` emits a static shell; `ssr` off means no server render attempts to touch wasm/Monaco.
export const ssr = false;
export const prerender = true;
