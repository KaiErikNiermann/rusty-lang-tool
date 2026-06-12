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
    const build = (b: typeof bytes): RltChecker =>
      // L3 confusion when the language has a model (en/de/ru/fr/es); plain L1+L2 otherwise (ar/it).
      b.confusion.length > 0
        ? mod.RltChecker.with_native_confusion(code, b.segmentSrx, b.tagger, b.disambig, b.grammar, b.confusion)
        : mod.RltChecker.with_native(code, b.segmentSrx, b.tagger, b.disambig, b.grammar);
    let checker: RltChecker;
    try {
      checker = build(bytes);
    } catch {
      // rkyv validation threw despite a hash match — evict + one forced re-fetch.
      await this.store.evictLanguage(code);
      checker = build(await this.store.ensureLanguage(code, signal, true));
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
