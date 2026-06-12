/// <reference lib="webworker" />
// The checker runs entirely off the main thread: wasm init, artifact fetch/verify/decompress (Cache
// Storage, crypto.subtle, DecompressionStream all work in workers), the heavy rkyv deserialize in
// `with_native*`, and every `check()`. The UI thread only sends text and renders the diagnostics that
// come back, so it never freezes on a 100 MB-language load or a large-input check.

import { ArtifactStore, createArtifactStore } from "../artifacts/store";
import { CheckerManager } from "./manager";
import type { FromWorker, ToWorker } from "./worker-protocol";

declare const self: DedicatedWorkerGlobalScope;

let store: ArtifactStore | null = null;
let manager: CheckerManager | null = null;

const post = (msg: FromWorker) => self.postMessage(msg);
const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

self.onmessage = async (event: MessageEvent<ToWorker>) => {
  const msg = event.data;
  switch (msg.type) {
    case "init": {
      try {
        store = await createArtifactStore(msg.manifestUrl, msg.baseUrl);
        store.state.subscribe((state) => post({ type: "state", state }));
        manager = new CheckerManager(store);
        post({ type: "inited", manifest: store.manifest });
      } catch (e) {
        post({ type: "init-error", message: errText(e) });
      }
      break;
    }
    case "select": {
      try {
        await manager!.select(msg.code);
        post({ type: "selected", reqId: msg.reqId });
      } catch (e) {
        post({ type: "error", reqId: msg.reqId, message: errText(e), retryable: true });
      }
      break;
    }
    case "check": {
      try {
        post({ type: "diagnostics", reqId: msg.reqId, diagnostics: manager!.check(msg.text) });
      } catch (e) {
        post({ type: "error", reqId: msg.reqId, message: errText(e), retryable: false });
      }
      break;
    }
  }
};
