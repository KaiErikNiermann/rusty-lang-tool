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

/**
 * A schemaVersion-1 artifact entry: gzip-only, with the variant fields inlined (no `gzip`/`brotli`
 * wrapper). A v1 entry is exactly a gzip {@link VariantRef} plus `rawBytes`, so it lifts losslessly to v2.
 */
interface V1ArtifactRef {
  asset: string;
  sha256: string;
  bytes: number;
  rawBytes: number;
}

/** The manifest as it arrives over the wire — either schema version. Normalized to v2 by {@link normalizeManifest}. */
export interface RawManifest {
  schemaVersion: number;
  ltVersion: string;
  shared: { "segment.srx": ArtifactRef | V1ArtifactRef };
  languages: Record<
    string,
    { label: string; totalBytes: number; files: Record<string, (ArtifactRef | V1ArtifactRef) | undefined> }
  >;
}

/** A v2 entry already carries `gzip`; a v1 entry has the variant fields inline. */
function isV2Ref(ref: ArtifactRef | V1ArtifactRef): ref is ArtifactRef {
  return "gzip" in ref;
}

/** Lift one entry to v2: a v1 (flat, gzip-only) entry becomes a v2 entry with just the `gzip` variant. */
function liftRef(ref: ArtifactRef | V1ArtifactRef): ArtifactRef {
  if (isV2Ref(ref)) return ref;
  return { gzip: { asset: ref.asset, sha256: ref.sha256, bytes: ref.bytes }, rawBytes: ref.rawBytes };
}

/**
 * Normalize a fetched manifest of either schema version into the canonical in-memory v2 shape, so the
 * reader is decoupled from the released manifest version. A v1 manifest (gzip-only) yields entries with
 * no `brotli` variant — the Fast track then falls back to gzip per artifact (see `ArtifactStore.variantFor`).
 * This keeps the site working when the deployed code is ahead of the latest artifact Release.
 */
export function normalizeManifest(raw: RawManifest): WebManifest {
  const liftFiles = (files: Record<string, (ArtifactRef | V1ArtifactRef) | undefined>): LangArtifacts["files"] =>
    Object.fromEntries(
      Object.entries(files).flatMap(([k, v]) => (v ? [[k, liftRef(v)] as const] : [])),
    ) as LangArtifacts["files"];
  return {
    schemaVersion: 2,
    ltVersion: raw.ltVersion,
    shared: { "segment.srx": liftRef(raw.shared["segment.srx"]) },
    languages: Object.fromEntries(
      Object.entries(raw.languages).map(([code, l]) => [
        code,
        { label: l.label, totalBytes: l.totalBytes, files: liftFiles(l.files) },
      ]),
    ),
  };
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
