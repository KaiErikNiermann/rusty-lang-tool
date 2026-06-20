import { writable, type Writable } from "svelte/store";

import type {
  ArtifactRef,
  Codec,
  FetchState,
  LangArtifacts,
  LangBytes,
  VariantRef,
  WebManifest,
} from "./types";

const CACHE_NAME = "rlt-artifacts-v1";
const MAX_ATTEMPTS = 3;

/** Inflates one artifact's compressed bytes to its raw `.rkyv`/text bytes. */
export type Decoder = (bytes: Uint8Array) => Promise<Uint8Array>;

/** Hex SHA-256 of a buffer, via Web Crypto. */
async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes as BufferSource);
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
}

/** Decompress gzip bytes via the native `DecompressionStream` (no extra deps) — the Reliable track. */
async function gunzip(bytes: Uint8Array): Promise<Uint8Array> {
  const ds = new DecompressionStream("gzip");
  const body = new Response(new Blob([bytes as BlobPart]).stream().pipeThrough(ds));
  return new Uint8Array(await body.arrayBuffer());
}

/** Download `url`, reporting each chunk's byte count for progress. Throws on non-200 / network error. */
async function download(
  url: string,
  onChunk: (delta: number) => void,
  signal?: AbortSignal,
): Promise<Uint8Array> {
  const res = await fetch(url, { signal: signal ?? null, cache: "no-store" });
  if (!res.ok) throw new Error(`HTTP ${res.status} for ${url}`);
  const reader = res.body?.getReader();
  if (!reader) throw new Error(`no response body for ${url}`);
  const chunks: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    if (value) {
      chunks.push(value);
      total += value.length;
      onChunk(value.length);
    }
  }
  const out = new Uint8Array(total);
  let off = 0;
  for (const c of chunks) {
    out.set(c, off);
    off += c.length;
  }
  return out;
}

const sleep = (ms: number, signal?: AbortSignal) =>
  new Promise<void>((resolve, reject) => {
    const t = setTimeout(resolve, ms);
    signal?.addEventListener("abort", () => {
      clearTimeout(t);
      reject(new DOMException("aborted", "AbortError"));
    });
  });

/** The raw-byte promises for one language's artifacts, all downloading concurrently. */
interface StagedFetch {
  srx: Promise<Uint8Array>;
  tagger: Promise<Uint8Array>;
  grammar: Promise<Uint8Array>;
  /** Resolves to an empty buffer when the language ships no disambiguation. */
  disambig: Promise<Uint8Array>;
  /** Resolves to an empty buffer when the language has no L3 confusion model. */
  confusion: Promise<Uint8Array>;
  /** Resolves once every artifact above has settled successfully (drives the "ready" state). */
  done: Promise<void>;
}

/**
 * Fetches, verifies, decompresses, and caches the per-language artifacts the WASM checker needs.
 *
 * Integrity: every downloaded asset's SHA-256 is checked against the manifest *before* it's cached and
 * decompressed, so a cache hit is trusted by construction. Corruption (bad hash, decompress failure,
 * network error) triggers bounded retry with backoff; a poisoned entry is evicted. The cache is keyed
 * by the compressed-variant hash, so a different codec (gzip vs brotli) or a bumped artifact changes
 * the key and clients re-fetch.
 *
 * Codec: the Reliable track fetches the `gzip` variant and inflates it with the browser-native
 * `DecompressionStream`; the Fast track fetches the smaller `brotli` variant and inflates it with the
 * injected `brotli` decoder (backed by our wasm). A `brotli` request silently falls back to `gzip`
 * when a variant or the decoder is unavailable — so the worker is never left without a path.
 */
export class ArtifactStore {
  /** Reactive fetch state for the UI (one store; reflects the in-flight language load). */
  readonly state: Writable<FetchState> = writable({ kind: "idle" });

  private readonly decoders: Record<Codec, Decoder | undefined>;

  constructor(
    readonly manifest: WebManifest,
    private readonly baseUrl: string,
    brotli?: Decoder,
  ) {
    this.decoders = { gzip: gunzip, brotli };
  }

  private url(asset: string): string {
    return `${this.baseUrl.replace(/\/+$/, "")}/${asset}`;
  }

  private cacheKey(sha256: string): Request {
    return new Request(`https://rlt.local/artifact/${sha256}`);
  }

  /**
   * Resolve which compressed variant to fetch for `ref` under `codec`, plus its decoder. Falls back to
   * gzip when brotli is requested but the variant or its decoder is missing — gzip always exists.
   */
  private variantFor(ref: ArtifactRef, codec: Codec): { variant: VariantRef; decode: Decoder } {
    if (codec === "brotli" && ref.brotli && this.decoders.brotli) {
      return { variant: ref.brotli, decode: this.decoders.brotli };
    }
    return { variant: ref.gzip, decode: gunzip };
  }

  /** Cached decompressed bytes for a verified variant, or `null`. */
  private async fromCache(sha256: string): Promise<Uint8Array | null> {
    const cache = await caches.open(CACHE_NAME);
    const hit = await cache.match(this.cacheKey(sha256));
    return hit ? new Uint8Array(await hit.arrayBuffer()) : null;
  }

  /** Download + verify + decompress + cache one artifact, with bounded retry. Returns raw `.rkyv` bytes. */
  private async fetchOne(
    ref: ArtifactRef,
    codec: Codec,
    onChunk: (delta: number) => void,
    signal?: AbortSignal,
    forceRefetch = false,
  ): Promise<Uint8Array> {
    const { variant, decode } = this.variantFor(ref, codec);
    if (!forceRefetch) {
      const cached = await this.fromCache(variant.sha256);
      if (cached) {
        onChunk(variant.bytes); // count a cache hit as fully "loaded" for the aggregate bar
        return cached;
      }
    }
    let lastErr: unknown;
    for (let attempt = 1; attempt <= MAX_ATTEMPTS; attempt++) {
      try {
        const comp = await download(this.url(variant.asset), onChunk, signal);
        this.state.set({ kind: "verifying", file: variant.asset });
        const got = await sha256Hex(comp);
        if (got !== variant.sha256) throw new Error(`integrity mismatch on ${variant.asset}`);
        const raw = await decode(comp);
        if (raw.length !== ref.rawBytes) {
          throw new Error(`size mismatch on ${variant.asset}: ${raw.length} != ${ref.rawBytes}`);
        }
        // Persistence is best-effort: a full quota disables caching (re-download next time) but never
        // breaks the checker — the verified bytes are returned regardless.
        try {
          const cache = await caches.open(CACHE_NAME);
          await cache.put(this.cacheKey(variant.sha256), new Response(raw as BodyInit));
        } catch (e) {
          console.warn(`could not cache ${variant.asset} (likely storage quota); will re-fetch later`, e);
        }
        return raw;
      } catch (err) {
        if (err instanceof DOMException && err.name === "AbortError") throw err;
        lastErr = err;
        await this.evict(variant.sha256); // never leave a partial/poisoned entry
        if (attempt < MAX_ATTEMPTS) {
          await sleep(250 * 2 ** attempt + Math.floor(Math.random() * 200), signal);
        }
      }
    }
    throw lastErr instanceof Error ? lastErr : new Error(String(lastErr));
  }

  /** Drop a content-addressed cache entry (called on a thrown rkyv error from the WASM constructor). */
  async evict(sha256: string): Promise<void> {
    const cache = await caches.open(CACHE_NAME);
    await cache.delete(this.cacheKey(sha256));
  }

  /** Remove cache entries whose hash is not referenced by the current manifest (either variant). */
  async gcStale(): Promise<void> {
    const live = new Set<string>();
    const addRef = (ref: ArtifactRef | undefined) => {
      if (!ref) return;
      live.add(ref.gzip.sha256);
      if (ref.brotli) live.add(ref.brotli.sha256);
    };
    addRef(this.manifest.shared["segment.srx"]);
    for (const lang of Object.values(this.manifest.languages)) {
      for (const ref of Object.values(lang.files)) addRef(ref);
    }
    const cache = await caches.open(CACHE_NAME);
    for (const req of await cache.keys()) {
      const hash = req.url.split("/").pop() ?? "";
      if (!live.has(hash)) await cache.delete(req);
    }
  }

  private refsFor(code: string): { srx: ArtifactRef; lang: LangArtifacts; all: ArtifactRef[] } {
    const lang = this.manifest.languages[code];
    if (!lang) throw new Error(`unknown language ${code}`);
    const srx = this.manifest.shared["segment.srx"];
    const all = [srx, lang.files["tagger.rkyv"], lang.files["grammar.rkyv"]];
    if (lang.files["disambig.rkyv"]) all.push(lang.files["disambig.rkyv"]);
    if (lang.files["confusion.rkyv"]) all.push(lang.files["confusion.rkyv"]);
    return { srx, lang, all };
  }

  /** Compressed (download) size of `ref` under `codec` — used to weight the aggregate progress bar. */
  private variantBytes(ref: ArtifactRef, codec: Codec): number {
    return this.variantFor(ref, codec).variant.bytes;
  }

  /**
   * Set up the shared aggregate progress bar for a `code` load and return a `get(ref)` that downloads +
   * verifies + decompresses one artifact while feeding that bar. The total is weighted by the chosen
   * codec's compressed sizes.
   */
  private progress(
    code: string,
    codec: Codec,
    signal: AbortSignal | undefined,
    forceRefetch: boolean,
  ): { lang: LangArtifacts; srx: ArtifactRef; get: (ref: ArtifactRef) => Promise<Uint8Array> } {
    const { srx, lang, all } = this.refsFor(code);
    const total = all.reduce((n, r) => n + this.variantBytes(r, codec), 0);
    let loaded = 0;
    const onChunk = (delta: number) => {
      loaded += delta;
      this.state.set({
        kind: "downloading",
        file: lang.label,
        loaded,
        total,
        pct: total ? Math.min(100, Math.round((loaded / total) * 100)) : 0,
      });
    };
    this.state.set({ kind: "downloading", file: lang.label, loaded: 0, total, pct: 0 });
    return { lang, srx, get: (ref) => this.fetchOne(ref, codec, onChunk, signal, forceRefetch) };
  }

  /** Mark all of a load's artifacts settled (or not) — flips the bar to "ready", swallowing rejection. */
  private settleOn(promises: Promise<unknown>[]): Promise<void> {
    const done = Promise.all(promises).then(() => {
      this.state.set({ kind: "ready" });
    });
    done.catch(() => {}); // real errors surface on the individual promises the caller awaits
    return done;
  }

  /**
   * Ensure every artifact for `code` is downloaded, verified, and cached, and return the bytes the
   * checker constructor consumes — the **monolithic** (Reliable-track) load: all files in parallel
   * (max throughput), resolved together. Idempotent; resolves from cache when present.
   */
  async ensureLanguage(
    code: string,
    codec: Codec = "gzip",
    signal?: AbortSignal,
    forceRefetch = false,
  ): Promise<LangBytes> {
    try {
      const { lang, srx, get } = this.progress(code, codec, signal, forceRefetch);
      const empty = Promise.resolve(new Uint8Array());
      const [srxBytes, tagger, grammar, disambig, confusion] = await Promise.all([
        get(srx),
        get(lang.files["tagger.rkyv"]),
        get(lang.files["grammar.rkyv"]),
        lang.files["disambig.rkyv"] ? get(lang.files["disambig.rkyv"]) : empty,
        lang.files["confusion.rkyv"] ? get(lang.files["confusion.rkyv"]) : empty,
      ]);
      this.state.set({ kind: "ready" });
      return { segmentSrx: new TextDecoder().decode(srxBytes), tagger, grammar, disambig, confusion };
    } catch (err) {
      if (err instanceof DOMException && err.name === "AbortError") throw err;
      const message = err instanceof Error ? err.message : String(err);
      this.state.set({ kind: "error", message, retryable: true });
      throw err;
    }
  }

  /**
   * Start a **staged** (Fast-track) load with **priority ordering**: the L1 group (tagger + shared srx +
   * optional disambig) downloads first, *then* grammar, *then* confusion. On a bandwidth-bound link this
   * is what makes progressive pay off — the tagger (the largest artifact, which spelling needs) completes
   * at its own size instead of finishing last in a parallel race, so spelling lights up well before the
   * later layers. The returned per-artifact promises let the caller build L1 → L2 → L3 as each group
   * lands. Errors surface on the individual promises the caller awaits.
   */
  startStaged(code: string, codec: Codec, signal?: AbortSignal, forceRefetch = false): StagedFetch {
    const { lang, srx, get } = this.progress(code, codec, signal, forceRefetch);
    const empty = Promise.resolve(new Uint8Array());
    // L1 group — kicked off immediately, in parallel among themselves (tagger dominates).
    const l1 = {
      srx: get(srx),
      tagger: get(lang.files["tagger.rkyv"]),
      disambig: lang.files["disambig.rkyv"] ? get(lang.files["disambig.rkyv"]) : empty,
    };
    const l1Done = Promise.all([l1.srx, l1.tagger, l1.disambig]);
    // Grammar + confusion both start once L1 has fully arrived — so they never steal the tagger's
    // bandwidth (the first-lint win), but then download in parallel with *each other* so the total-load
    // penalty over a strict serial chain stays small.
    const grammar = l1Done.then(() => get(lang.files["grammar.rkyv"]));
    const confusion = lang.files["confusion.rkyv"]
      ? l1Done.then(() => get(lang.files["confusion.rkyv"] as ArtifactRef))
      : empty;
    const done = this.settleOn([l1Done, grammar, confusion]);
    return { ...l1, grammar, confusion, done };
  }

  /** Flag an in-flight staged load as failed, for the UI. */
  reportError(message: string): void {
    this.state.set({ kind: "error", message, retryable: true });
  }

  /** Drop every cached file (both variants) for a language (before a forced re-fetch after a bad load). */
  async evictLanguage(code: string): Promise<void> {
    const { all } = this.refsFor(code);
    await Promise.all(
      all.flatMap((r) => [this.evict(r.gzip.sha256), ...(r.brotli ? [this.evict(r.brotli.sha256)] : [])]),
    );
  }
}

/** Load the manifest baked into the deployed site, then build a store pointed at the artifact host. */
export async function createArtifactStore(
  manifestUrl: string,
  baseUrl: string,
  brotli?: Decoder,
): Promise<ArtifactStore> {
  const res = await fetch(manifestUrl, { cache: "no-store" });
  if (!res.ok) throw new Error(`could not load artifact manifest (${res.status})`);
  const manifest = (await res.json()) as WebManifest;
  const store = new ArtifactStore(manifest, baseUrl, brotli);
  void store.gcStale();
  try {
    await navigator.storage?.persist?.();
  } catch {
    /* best-effort; quota errors surface later as a clean fetch error */
  }
  return store;
}
