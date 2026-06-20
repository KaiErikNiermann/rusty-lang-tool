import type { FetchState, LoadPlan, WebManifest } from "../artifacts/types";
import type { StageLayer } from "./manager";
import type { Diagnostic } from "./types";

/** Messages the main thread sends to the checker worker. */
export type ToWorker =
  | { type: "init"; manifestUrl: string; baseUrl: string }
  | { type: "select"; reqId: number; code: string; plan: LoadPlan; rebuild: boolean }
  | { type: "check"; reqId: number; text: string };

/** Messages the checker worker sends back. `reqId` echoes the request it answers. */
export type FromWorker =
  | { type: "inited"; manifest: WebManifest }
  | { type: "init-error"; message: string }
  | { type: "state"; state: FetchState }
  /** A cascade layer became active mid-load (progressive); the main thread re-checks on each. */
  | { type: "stage"; reqId: number; layer: StageLayer }
  | { type: "selected"; reqId: number }
  | { type: "diagnostics"; reqId: number; diagnostics: Diagnostic[] }
  | { type: "error"; reqId: number; message: string; retryable: boolean };
