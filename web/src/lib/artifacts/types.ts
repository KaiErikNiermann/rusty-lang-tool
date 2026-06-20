// Mirrors the `web-artifacts.json` emitted by `cargo xtask web-manifest` — the browser's root of trust.

/**
 * One compressed encoding of an artifact: the flat Release asset name + the SHA-256 of the
 * *compressed* bytes (verified after download, before decompression; also the cache key). Each
 * artifact ships two of these — a `gzip` variant (the Reliable track, inflated by the browser's
 * native `DecompressionStream`) and an optional `brotli` variant (the Fast track, ≈32% smaller,
 * inflated by `brotli_decompress` in our wasm).
 */
export interface VariantRef {
  /** Flat asset filename on the Release (`en-tagger.rkyv.gz` / `en-tagger.rkyv.br`). */
  asset: string;
  /** Hex SHA-256 of the compressed bytes. Verified after download; also the content-addressed cache key. */
  sha256: string;
  /** Compressed (downloaded) size in bytes. */
  bytes: number;
}

/** One artifact, in every encoding we publish, plus its decompressed size (codec-independent). */
export interface ArtifactRef {
  gzip: VariantRef;
  /** Present when the Fast (beta) track is available for this artifact. */
  brotli?: VariantRef;
  /** Decompressed `.rkyv` size in bytes — a sanity check after decompression. */
  rawBytes: number;
}

/** One language's `with_native` artifacts. */
export interface LangArtifacts {
  label: string;
  totalBytes: number;
  files: {
    "tagger.rkyv": ArtifactRef;
    "grammar.rkyv": ArtifactRef;
    "disambig.rkyv"?: ArtifactRef;
    /** L3 real-word-error model (en/de/ru/fr/es); absent for ar/it. */
    "confusion.rkyv"?: ArtifactRef;
  };
}

/** The full manifest. v2 adds the optional per-artifact `brotli` variant (v1 was gzip-only). */
export interface WebManifest {
  schemaVersion: 2;
  ltVersion: string;
  shared: { "segment.srx": ArtifactRef };
  languages: Record<string, LangArtifacts>;
}

/** Which compressed encoding a download track fetches + decompresses. */
export type Codec = "gzip" | "brotli";

/**
 * How the web demo loads a language. Two presets ship (see `config.ts`):
 * - **Reliable** `{ codec: "gzip", staged: false }` — browser-native gunzip, one monolithic build.
 *   The dependable default; works with zero extra wasm surface.
 * - **Fast (beta)** `{ codec: "brotli", staged: true }` — ≈32% smaller brotli artifacts decoded in
 *   wasm, and **progressive** construction (spelling lights up the instant the tagger arrives, then
 *   grammar, then confusion stream in) so time-to-first-lint drops sharply.
 */
export interface LoadPlan {
  codec: Codec;
  staged: boolean;
}

/** The raw bytes a `RltChecker.with_native*` constructor consumes for one language. */
export interface LangBytes {
  /** Decoded UTF-8 segment.srx (the constructor takes a string). */
  segmentSrx: string;
  tagger: Uint8Array;
  /** Empty when the language ships no disambiguation. */
  disambig: Uint8Array;
  grammar: Uint8Array;
  /** Empty when the language has no L3 confusion model (ar/it). */
  confusion: Uint8Array;
}

/** Progress/health of fetching a language's artifacts, surfaced to the UI. */
export type FetchState =
  | { kind: "idle" }
  | { kind: "downloading"; file: string; loaded: number; total: number; pct: number }
  | { kind: "verifying"; file: string }
  | { kind: "ready" }
  | { kind: "error"; message: string; retryable: boolean };
