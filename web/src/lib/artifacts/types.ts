// Mirrors the `web-artifacts.json` emitted by `cargo xtask web-manifest` — the browser's root of trust.

/** One compressed artifact: the flat Release asset name + the SHA-256 of the *compressed* bytes. */
export interface ArtifactRef {
  /** Flat asset filename on the Release (`en-tagger.rkyv.gz`); fetched as `<base>/<asset>`. */
  asset: string;
  /** Hex SHA-256 of the gzipped bytes — verified after download, before decompression. Also the cache key. */
  sha256: string;
  /** Compressed (downloaded) size in bytes. */
  bytes: number;
  /** Decompressed `.rkyv` size in bytes — a sanity check after `DecompressionStream`. */
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
  };
}

/** The full manifest. */
export interface WebManifest {
  schemaVersion: 1;
  ltVersion: string;
  shared: { "segment.srx": ArtifactRef };
  languages: Record<string, LangArtifacts>;
}

/** The raw bytes a `RltChecker.with_native` constructor consumes for one language. */
export interface LangBytes {
  /** Decoded UTF-8 segment.srx (the constructor takes a string). */
  segmentSrx: string;
  tagger: Uint8Array;
  /** Empty when the language ships no disambiguation. */
  disambig: Uint8Array;
  grammar: Uint8Array;
}

/** Progress/health of fetching a language's artifacts, surfaced to the UI. */
export type FetchState =
  | { kind: "idle" }
  | { kind: "downloading"; file: string; loaded: number; total: number; pct: number }
  | { kind: "verifying"; file: string }
  | { kind: "ready" }
  | { kind: "error"; message: string; retryable: boolean };
