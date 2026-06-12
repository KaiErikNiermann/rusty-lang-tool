import { writable, type Writable } from "svelte/store";

import type { ArtifactRef, FetchState, LangArtifacts, LangBytes, WebManifest } from "./types";

const CACHE_NAME = "rlt-artifacts-v1";
const MAX_ATTEMPTS = 3;

/** Hex SHA-256 of a buffer, via Web Crypto. */
async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes as BufferSource);
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
}

/** Decompress gzip bytes via the native `DecompressionStream` (no extra deps). */
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

/**
 * Fetches, verifies, decompresses, and caches the per-language artifacts the WASM checker needs.
 *
 * Integrity: every downloaded asset's SHA-256 is checked against the manifest *before* it's cached and
 * decompressed, so a cache hit is trusted by construction. Corruption (bad hash, decompress failure,
 * network error) triggers bounded retry with backoff; a poisoned entry is evicted. The cache is keyed
 * by the compressed-asset hash, so bumping an artifact changes the key and clients re-fetch.
 */
export class ArtifactStore {
  /** Reactive fetch state for the UI (one store; reflects the in-flight language load). */
  readonly state: Writable<FetchState> = writable({ kind: "idle" });

  constructor(
    readonly manifest: WebManifest,
    private readonly baseUrl: string,
  ) {}

  private url(asset: string): string {
    return `${this.baseUrl.replace(/\/+$/, "")}/${asset}`;
  }

  private cacheKey(sha256: string): Request {
    return new Request(`https://rlt.local/artifact/${sha256}`);
  }

  /** Cached decompressed bytes for a verified artifact, or `null`. */
  private async fromCache(ref: ArtifactRef): Promise<Uint8Array | null> {
    const cache = await caches.open(CACHE_NAME);
    const hit = await cache.match(this.cacheKey(ref.sha256));
    return hit ? new Uint8Array(await hit.arrayBuffer()) : null;
  }

  /** Download + verify + decompress + cache one artifact, with bounded retry. Returns raw `.rkyv` bytes. */
  private async fetchOne(
    ref: ArtifactRef,
    onChunk: (delta: number) => void,
    signal?: AbortSignal,
    forceRefetch = false,
  ): Promise<Uint8Array> {
    if (!forceRefetch) {
      const cached = await this.fromCache(ref);
      if (cached) {
        onChunk(ref.bytes); // count a cache hit as fully "loaded" for the aggregate bar
        return cached;
      }
    }
    let lastErr: unknown;
    for (let attempt = 1; attempt <= MAX_ATTEMPTS; attempt++) {
      try {
        const gz = await download(this.url(ref.asset), onChunk, signal);
        this.state.set({ kind: "verifying", file: ref.asset });
        const got = await sha256Hex(gz);
        if (got !== ref.sha256) throw new Error(`integrity mismatch on ${ref.asset}`);
        const raw = await gunzip(gz);
        if (raw.length !== ref.rawBytes) {
          throw new Error(`size mismatch on ${ref.asset}: ${raw.length} != ${ref.rawBytes}`);
        }
        const cache = await caches.open(CACHE_NAME);
        await cache.put(this.cacheKey(ref.sha256), new Response(raw as BodyInit));
        return raw;
      } catch (err) {
        if (err instanceof DOMException && err.name === "AbortError") throw err;
        lastErr = err;
        await this.evict(ref.sha256); // never leave a partial/poisoned entry
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

  /** Remove cache entries whose hash is not referenced by the current manifest. */
  async gcStale(): Promise<void> {
    const live = new Set<string>();
    live.add(this.manifest.shared["segment.srx"].sha256);
    for (const lang of Object.values(this.manifest.languages)) {
      for (const ref of Object.values(lang.files)) if (ref) live.add(ref.sha256);
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
    return { srx, lang, all };
  }

  /**
   * Ensure every artifact for `code` (+ shared srx) is downloaded, verified, and cached, and return the
   * bytes the checker constructor consumes. Idempotent; resolves from cache when present. Files are
   * fetched in parallel with an aggregate progress bar (weighted by compressed size).
   */
  async ensureLanguage(code: string, signal?: AbortSignal, forceRefetch = false): Promise<LangBytes> {
    const { srx, lang, all } = this.refsFor(code);
    const total = all.reduce((n, r) => n + r.bytes, 0);
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
    try {
      this.state.set({ kind: "downloading", file: lang.label, loaded: 0, total, pct: 0 });
      const get = (ref: ArtifactRef) => this.fetchOne(ref, onChunk, signal, forceRefetch);
      const [srxBytes, tagger, grammar, disambig] = await Promise.all([
        get(srx),
        get(lang.files["tagger.rkyv"]),
        get(lang.files["grammar.rkyv"]),
        lang.files["disambig.rkyv"] ? get(lang.files["disambig.rkyv"]) : Promise.resolve(new Uint8Array()),
      ]);
      this.state.set({ kind: "ready" });
      return { segmentSrx: new TextDecoder().decode(srxBytes), tagger, grammar, disambig };
    } catch (err) {
      if (err instanceof DOMException && err.name === "AbortError") throw err;
      const message = err instanceof Error ? err.message : String(err);
      this.state.set({ kind: "error", message, retryable: true });
      throw err;
    }
  }

  /** Drop every cached file for a language (used before a forced re-fetch after a WASM-load failure). */
  async evictLanguage(code: string): Promise<void> {
    const { all } = this.refsFor(code);
    await Promise.all(all.map((r) => this.evict(r.sha256)));
  }
}

/** Load the manifest baked into the deployed site, then build a store pointed at the artifact host. */
export async function createArtifactStore(manifestUrl: string, baseUrl: string): Promise<ArtifactStore> {
  const res = await fetch(manifestUrl, { cache: "no-store" });
  if (!res.ok) throw new Error(`could not load artifact manifest (${res.status})`);
  const manifest = (await res.json()) as WebManifest;
  const store = new ArtifactStore(manifest, baseUrl);
  void store.gcStale();
  try {
    await navigator.storage?.persist?.();
  } catch {
    /* best-effort; quota errors surface later as a clean fetch error */
  }
  return store;
}
