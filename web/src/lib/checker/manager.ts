import type { RltChecker } from "$wasm";

import type { ArtifactStore } from "../artifacts/store";
import type { Diagnostic } from "./types";
import { initWasm } from "./wasm";

/**
 * Owns at most one live `RltChecker`. Selecting a language loads its artifacts (via the store), builds
 * the native checker once, and frees the previous one's wasm memory. A construction failure — rkyv
 * validation throwing despite a hash match — evicts the cached files and retries once with a forced
 * re-fetch (the last line of defense against a bad download), before surfacing the error.
 */
export class CheckerManager {
  private current: { code: string; checker: RltChecker } | null = null;

  constructor(private readonly store: ArtifactStore) {}

  get language(): string | null {
    return this.current?.code ?? null;
  }

  async select(code: string, signal?: AbortSignal): Promise<void> {
    if (this.current?.code === code) return;
    const [mod, bytes] = await Promise.all([initWasm(), this.store.ensureLanguage(code, signal)]);
    let checker: RltChecker;
    try {
      checker = mod.RltChecker.with_native(
        code,
        bytes.segmentSrx,
        bytes.tagger,
        bytes.disambig,
        bytes.grammar,
      );
    } catch {
      await this.store.evictLanguage(code);
      const fresh = await this.store.ensureLanguage(code, signal, true);
      checker = mod.RltChecker.with_native(
        code,
        fresh.segmentSrx,
        fresh.tagger,
        fresh.disambig,
        fresh.grammar,
      );
    }
    this.current?.checker.free();
    this.current = { code, checker };
  }

  /** Run the cascade over `text`. Throws if no language is selected. */
  check(text: string): Diagnostic[] {
    if (!this.current) throw new Error("no language selected");
    return this.current.checker.check(text) as Diagnostic[];
  }

  dispose(): void {
    this.current?.checker.free();
    this.current = null;
  }
}
