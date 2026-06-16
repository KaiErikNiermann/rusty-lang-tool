import type { RltChecker } from "$wasm";

import type { ArtifactStore } from "../artifacts/store";
import type { Diagnostic } from "./types";
import { initWasm } from "./wasm";

/**
 * How many built `RltChecker`s to keep alive at once. Building a checker is ~1s of wasm work even from
 * cached artifacts (full rkyv deserialize + compiling every grammar/disambiguation rule's regexes +
 * the tagger FST), so pooling the most-recently-used ones makes switching *back* to a language instant.
 * They all share one wasm linear memory, so each extra live checker costs real memory (the big
 * languages — ru/de/ar — are heavy); 3 covers the common "toggle between two or three" case without
 * holding the whole set.
 */
const MAX_LIVE_CHECKERS = 3;

/**
 * Owns an LRU pool of live `RltChecker`s. Selecting a pooled language activates it instantly; selecting
 * a new one builds it (loading artifacts via the store) and evicts the least-recently-used checker once
 * over [`MAX_LIVE_CHECKERS`], freeing its wasm memory. A construction failure — rkyv validation throwing
 * despite a hash match — evicts the cached files and retries once with a forced re-fetch (the last line
 * of defense against a bad download) before surfacing the error.
 */
export class CheckerManager {
  /** Insertion order is the LRU order: oldest (next to evict) first, most-recently-used last. */
  private readonly pool = new Map<string, RltChecker>();
  private activeCode: string | null = null;

  constructor(private readonly store: ArtifactStore) {}

  get language(): string | null {
    return this.activeCode;
  }

  async select(code: string, signal?: AbortSignal): Promise<void> {
    // Warm hit: a previously-built checker is still pooled → activate it without rebuilding.
    const pooled = this.pool.get(code);
    if (pooled) {
      this.pool.delete(code);
      this.pool.set(code, pooled); // re-insert as most-recently-used
      this.activeCode = code;
      return;
    }

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
    this.pool.set(code, checker);
    this.activeCode = code;
    this.evictExcess(code);
  }

  /** Free the least-recently-used checkers beyond the cap (never the one just activated). */
  private evictExcess(keep: string): void {
    while (this.pool.size > MAX_LIVE_CHECKERS) {
      const oldest = this.pool.keys().next().value;
      if (oldest === undefined || oldest === keep) break;
      this.pool.get(oldest)?.free();
      this.pool.delete(oldest);
    }
  }

  /** Run the cascade over `text`. Throws if no language is selected. */
  check(text: string): Diagnostic[] {
    const active = this.activeCode ? this.pool.get(this.activeCode) : undefined;
    if (!active) throw new Error("no language selected");
    return active.check(text) as Diagnostic[];
  }

  dispose(): void {
    for (const checker of this.pool.values()) checker.free();
    this.pool.clear();
    this.activeCode = null;
  }
}
