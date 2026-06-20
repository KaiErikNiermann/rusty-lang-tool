import type { RltChecker } from "$wasm";

import type { ArtifactStore } from "../artifacts/store";
import type { LoadPlan } from "../artifacts/types";
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
 * A cascade layer reaching readiness during a load. `"all"` is the single notification of a monolithic
 * (Reliable-track) build; the staged (Fast-track) build emits `"spelling"`, then `"grammar"`, then
 * (when the language has an L3 model) `"confusion"` as each becomes active. The UI re-runs `check()` on
 * each so newly-available diagnostics surface progressively.
 */
export type StageLayer = "spelling" | "grammar" | "confusion" | "all";

/** Called as each layer becomes active so the UI can flip to interactive and re-check. */
export type OnStage = (layer: StageLayer) => void;

/**
 * Owns an LRU pool of live `RltChecker`s. Selecting a pooled language activates it instantly; selecting
 * a new one builds it (loading artifacts via the store) and evicts the least-recently-used checker once
 * over [`MAX_LIVE_CHECKERS`], freeing its wasm memory.
 *
 * The load follows the active [`LoadPlan`]: **monolithic** (one `with_native*` call once every artifact
 * is in hand) or **staged/progressive** (build an L1-spelling-only checker the instant the tagger
 * lands, then re-build in place as L2 grammar and L3 confusion stream in). A construction failure —
 * rkyv validation throwing despite a hash match — evicts the cached files and retries once with a
 * forced re-fetch (the last line of defense against a bad download) before surfacing the error.
 */
export class CheckerManager {
  /** Insertion order is the LRU order: oldest (next to evict) first, most-recently-used last. */
  private readonly pool = new Map<string, RltChecker>();
  private activeCode: string | null = null;

  constructor(private readonly store: ArtifactStore) {}

  get language(): string | null {
    return this.activeCode;
  }

  async select(
    code: string,
    plan: LoadPlan,
    onStage?: OnStage,
    signal?: AbortSignal,
    rebuild = false,
  ): Promise<void> {
    // Warm hit: a previously-built checker is still pooled → activate it without rebuilding. Skipped on
    // an explicit `rebuild` (a track toggle) so the user sees the newly-selected load path run.
    const pooled = this.pool.get(code);
    if (pooled && !rebuild) {
      this.pool.delete(code);
      this.pool.set(code, pooled); // re-insert as most-recently-used
      this.activeCode = code;
      onStage?.("all");
      return;
    }

    try {
      await this.load(code, plan, onStage, signal, false);
    } catch (err) {
      if (err instanceof DOMException && err.name === "AbortError") throw err;
      // rkyv validation threw despite a hash match (or a transient bad blob) — evict + one forced refetch.
      await this.store.evictLanguage(code);
      await this.load(code, plan, onStage, signal, true);
    }
  }

  /** Run one full load for `code` under `plan`, dispatching to the staged or monolithic path. */
  private async load(
    code: string,
    plan: LoadPlan,
    onStage: OnStage | undefined,
    signal: AbortSignal | undefined,
    force: boolean,
  ): Promise<void> {
    const mod = await initWasm();
    if (plan.staged) {
      await this.loadStaged(mod, code, plan, onStage, signal, force);
    } else {
      const bytes = await this.store.ensureLanguage(code, plan.codec, signal, force);
      const checker =
        bytes.confusion.length > 0
          ? mod.RltChecker.with_native_confusion(
              code,
              bytes.segmentSrx,
              bytes.tagger,
              bytes.disambig,
              bytes.grammar,
              bytes.confusion,
            )
          : mod.RltChecker.with_native(code, bytes.segmentSrx, bytes.tagger, bytes.disambig, bytes.grammar);
      this.install(code, checker);
      onStage?.("all");
    }
  }

  /**
   * Progressive build: all artifacts download concurrently, but the checker is constructed (and
   * re-constructed in place) as each layer's inputs arrive — spelling first, then grammar, then
   * confusion. Each `install` swaps the pooled checker and frees the prior one, so `check()` always
   * runs the most capable cascade built so far.
   */
  private async loadStaged(
    mod: Awaited<ReturnType<typeof initWasm>>,
    code: string,
    plan: LoadPlan,
    onStage: OnStage | undefined,
    signal: AbortSignal | undefined,
    force: boolean,
  ): Promise<void> {
    const hasConfusion = !!this.store.manifest.languages[code]?.files["confusion.rkyv"];
    const f = this.store.startStaged(code, plan.codec, signal, force);
    try {
      // L1 — spelling lights up the moment the tagger (+ shared srx, + optional disambig) lands.
      const [srx, tagger, disambig] = await Promise.all([f.srx, f.tagger, f.disambig]);
      const segmentSrx = new TextDecoder().decode(srx);
      this.install(code, mod.RltChecker.with_native_spelling(code, segmentSrx, tagger, disambig));
      onStage?.("spelling");

      // L2 — grammar.
      const grammar = await f.grammar;
      this.install(code, mod.RltChecker.with_native(code, segmentSrx, tagger, disambig, grammar));
      onStage?.("grammar");

      // L3 — confusion, when the language ships a model.
      if (hasConfusion) {
        const confusion = await f.confusion;
        this.install(
          code,
          mod.RltChecker.with_native_confusion(code, segmentSrx, tagger, disambig, grammar, confusion),
        );
        onStage?.("confusion");
      }
      await f.done; // surface a late "ready"/error consistently with the monolithic path
    } catch (err) {
      if (!(err instanceof DOMException && err.name === "AbortError")) {
        this.store.reportError(err instanceof Error ? err.message : String(err));
      }
      throw err;
    }
  }

  /** Pool `checker` as the active language, freeing any checker it replaces, and trim the LRU. */
  private install(code: string, checker: RltChecker): void {
    const prev = this.pool.get(code);
    this.pool.set(code, checker);
    this.activeCode = code;
    if (prev && prev !== checker) prev.free(); // a staged upgrade replaced an earlier-layer checker
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
