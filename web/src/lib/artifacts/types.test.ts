import { describe, expect, it } from "vitest";

import { normalizeManifest, type RawManifest } from "./types";

// Regression for the Pages breakage "can't access property sha256, e.gzip is undefined": the deployed
// v2 reader was handed a schemaVersion-1 (gzip-only, flat) manifest from an older artifact Release.
// normalizeManifest must lift v1 entries to the v2 shape so the reader is decoupled from the release version.
describe("normalizeManifest", () => {
  it("lifts a schemaVersion-1 (flat, gzip-only) manifest to v2", () => {
    const v1: RawManifest = {
      schemaVersion: 1,
      ltVersion: "v6.7",
      shared: { "segment.srx": { asset: "segment.srx.gz", sha256: "aa", bytes: 10, rawBytes: 100 } },
      languages: {
        en: {
          label: "English",
          totalBytes: 30,
          files: {
            "tagger.rkyv": { asset: "en-tagger.rkyv.gz", sha256: "bb", bytes: 20, rawBytes: 200 },
            "grammar.rkyv": { asset: "en-grammar.rkyv.gz", sha256: "cc", bytes: 30, rawBytes: 300 },
          },
        },
      },
    };

    const m = normalizeManifest(v1);

    expect(m.schemaVersion).toBe(2);
    expect(m.shared["segment.srx"]).toEqual({
      gzip: { asset: "segment.srx.gz", sha256: "aa", bytes: 10 },
      rawBytes: 100,
    });
    const tagger = m.languages.en?.files["tagger.rkyv"];
    expect(tagger).toEqual({ gzip: { asset: "en-tagger.rkyv.gz", sha256: "bb", bytes: 20 }, rawBytes: 200 });
    // A v1 entry carries no brotli variant → the Fast track falls back to gzip per artifact.
    expect(tagger?.brotli).toBeUndefined();
  });

  it("passes a v2 (gzip+brotli) entry through unchanged", () => {
    const v2: RawManifest = {
      schemaVersion: 2,
      ltVersion: "v6.7",
      shared: {
        "segment.srx": {
          gzip: { asset: "segment.srx.gz", sha256: "aa", bytes: 10 },
          brotli: { asset: "segment.srx.br", sha256: "aabr", bytes: 7 },
          rawBytes: 100,
        },
      },
      languages: {},
    };

    const m = normalizeManifest(v2);

    expect(m.shared["segment.srx"].brotli).toEqual({ asset: "segment.srx.br", sha256: "aabr", bytes: 7 });
  });
});
