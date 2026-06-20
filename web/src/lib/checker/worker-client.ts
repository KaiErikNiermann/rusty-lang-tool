import { writable, type Writable } from "svelte/store";

import type { FetchState, LoadPlan, WebManifest } from "../artifacts/types";
import type { StageLayer } from "./manager";
import type { Diagnostic } from "./types";
import type { FromWorker, ToWorker } from "./worker-protocol";

interface Pending {
  resolve: (diags: Diagnostic[] | void) => void;
  reject: (err: Error) => void;
  /** For a `select`, called as each cascade layer becomes active (progressive load). */
  onStage?: (layer: StageLayer) => void;
}

/**
 * Main-thread handle to the checker worker. Mirrors the old in-thread manager (select/check) but every
 * call is async (a postMessage round-trip), so the UI never blocks. Fetch progress flows back through
 * the `state` store; in-flight requests are correlated by a monotonic id.
 */
export class WorkerChecker {
  readonly state: Writable<FetchState> = writable({ kind: "idle" });
  private worker: Worker;
  private nextId = 1;
  private pending = new Map<number, Pending>();
  private initResolve: ((m: WebManifest) => void) | null = null;
  private initReject: ((e: Error) => void) | null = null;

  constructor() {
    this.worker = new Worker(new URL("./checker.worker.ts", import.meta.url), { type: "module" });
    this.worker.onmessage = (e: MessageEvent<FromWorker>) => this.handle(e.data);
  }

  private handle(msg: FromWorker): void {
    switch (msg.type) {
      case "state":
        this.state.set(msg.state);
        break;
      case "inited":
        this.initResolve?.(msg.manifest);
        this.initResolve = this.initReject = null;
        break;
      case "init-error":
        this.initReject?.(new Error(msg.message));
        this.initResolve = this.initReject = null;
        break;
      case "stage":
        this.pending.get(msg.reqId)?.onStage?.(msg.layer);
        break;
      case "selected":
        this.pending.get(msg.reqId)?.resolve();
        this.pending.delete(msg.reqId);
        break;
      case "diagnostics":
        this.pending.get(msg.reqId)?.resolve(msg.diagnostics);
        this.pending.delete(msg.reqId);
        break;
      case "error":
        this.pending.get(msg.reqId)?.reject(new Error(msg.message));
        this.pending.delete(msg.reqId);
        break;
    }
  }

  private send(msg: ToWorker): void {
    this.worker.postMessage(msg);
  }

  /** Load the manifest + spin up the store inside the worker. Resolves with the manifest. */
  init(manifestUrl: string, baseUrl: string): Promise<WebManifest> {
    return new Promise<WebManifest>((resolve, reject) => {
      this.initResolve = resolve;
      this.initReject = reject;
      this.send({ type: "init", manifestUrl, baseUrl });
    });
  }

  /**
   * Load + build the checker for `code` under `plan` (fetches artifacts as needed). `onStage` fires as
   * each cascade layer becomes active — once (`"all"`) for a monolithic load, or progressively
   * (`"spelling"` → `"grammar"` → `"confusion"`) for a staged load — so the UI can re-check as
   * capabilities come online. The promise resolves when the load is fully complete.
   */
  select(
    code: string,
    plan: LoadPlan,
    onStage?: (layer: StageLayer) => void,
    rebuild = false,
  ): Promise<void> {
    const reqId = this.nextId++;
    return new Promise<void>((resolve, reject) => {
      const entry: Pending = { resolve: () => resolve(), reject };
      if (onStage) entry.onStage = onStage;
      this.pending.set(reqId, entry);
      this.send({ type: "select", reqId, code, plan, rebuild });
    });
  }

  /** Check `text`; resolves with the diagnostics. */
  check(text: string): Promise<Diagnostic[]> {
    const reqId = this.nextId++;
    return new Promise<Diagnostic[]>((resolve, reject) => {
      this.pending.set(reqId, { resolve: (d) => resolve((d as Diagnostic[] | undefined) ?? []), reject });
      this.send({ type: "check", reqId, text });
    });
  }

  dispose(): void {
    this.worker.terminate();
    this.pending.clear();
  }
}
