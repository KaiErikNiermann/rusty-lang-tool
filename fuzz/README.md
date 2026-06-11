# Fuzzing (`rlt-fuzz`)

Coverage-guided fuzzing of the trust boundaries and hottest parsing/matching code, via
[`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) + libFuzzer. This is a **separate
workspace** (note the empty `[workspace]` table in `Cargo.toml`) so the libFuzzer/sanitizer
dependencies never enter the normal build.

## Setup

```sh
cargo install cargo-fuzz   # one-time; needs a nightly toolchain + a C++ compiler
```

The targets depend on `rlt-ir` / `rlt-core` built with their optional `arbitrary` feature, which
derives `Arbitrary` on the IR and analysis types for structure-aware input generation. That feature
is off in every normal/runtime build.

## Targets

| Target | What it guards | Input |
|---|---|---|
| `ir_rkyv` | **Untrusted input.** The runtime loads the rule blob (`resources/en.rkyv`), in a web build from a URL. Malformed bytes must be rejected by rkyv's validation, never panic or read OOB. Also fuzzes rule **compilation** (regex building, suggestion pre-compile) on valid-but-hostile archives. | raw `&[u8]` |
| `confusion_rkyv` | Same trust boundary for the L3 confusion model (`resources/confusion.rkyv`). | raw `&[u8]` |
| `ir_match` | The L2 matcher over arbitrary rules + an arbitrary token graph: regex compilation, the backtracking matcher (`min`/`max`/`skip`), antipattern suppression, marker-span resolution, and the byte-offset **suggestion rendering** (`text.get(span)` / token-span slicing) against spans that need not correspond to the text. | `Arbitrary` `{ rules, analysis, text }` |
| `confusion_check` | The L3 checker: bigram / POS-context log-ratio lookups, neighbour selection, the contraction/evidence guards, and recase + span emission. | `Arbitrary` `{ model, analysis, text }` |
| `tagger_load` | **Untrusted input.** The native engine's POS tagger artifact (`resources/tagger.rkyv`), loaded in a web build from a URL — rkyv + embedded-fst validation must reject malformed bytes. | raw `&[u8]` |
| `disambig_rkyv` | Same boundary for the native disambiguation artifact (`resources/disambig.rkyv`); also fuzzes rule **compilation** (regex building). | raw `&[u8]` |
| `engine_analyze` | The native engine's `analyze` over arbitrary Unicode: segmentation, the word tokenizer, FST tagging, structural tagging (CD/PCT/NNP/SENT_START/SENT_END) and disambiguation. Asserts every token span is in-bounds, on a char boundary, and equals its source text. Loads the real artifacts once (no-ops if absent). | `Arbitrary` `String` |
| `engine_analyze_de` | Same invariants for the **German** engine — additionally exercises the STTS tagset and the **compound splitter**, whose longest-match byte arithmetic runs over multibyte UTF-8 (umlauts). The richest new UB surface for the second language. | `Arbitrary` `String` |
| `disambig_apply` | The disambiguation pass over arbitrary rules + an arbitrary token graph: regex compilation, marker resolution, the reused backtracking matcher, and tag-action application (retain/push over tags & lemmas). | `Arbitrary` `{ rules, tokens }` |

The `*_rkyv` + `tagger_load` targets are the security-relevant boundary (deserialising
attacker-controlled bytes). The structure-aware + `engine_analyze` targets exercise the most
error-prone runtime logic — regex/backtracking and byte-offset span arithmetic.

## Running

```sh
cargo fuzz list
cargo fuzz run ir_match -- -max_total_time=60          # bounded run
cargo fuzz run ir_rkyv  -- -max_total_time=60 -max_len=2000000
# or via the workspace task runner:
cargo xtask fuzz                                        # list targets
cargo xtask fuzz ir_match -- -max_total_time=60
```

A finding is written to `fuzz/artifacts/<target>/crash-*`; reproduce it with
`cargo fuzz run <target> fuzz/artifacts/<target>/crash-…`.

## Seeding the rkyv corpora (optional, recommended)

`corpus/` is git-ignored. Mutating a *valid* archive reaches far more of the deserialise path than
random bytes, so seed the two `*_rkyv` corpora from the real blobs once you've built them
(`cargo xtask build-blob` / `build-confusion`):

```sh
mkdir -p corpus/ir_rkyv corpus/confusion_rkyv
cp ../resources/en.rkyv        corpus/ir_rkyv/
cp ../resources/confusion.rkyv corpus/confusion_rkyv/
```

Run those targets with a `-max_len` at least as large as the seed.

## Status

Last sweep: all eight targets ran clean — no crashes, panics, or OOMs. The original four
(`ir_match` 0.58M execs, `confusion_check` 2.1M, `ir_rkyv` 1.5M, `confusion_rkyv` 72.8M) plus the
native-engine four (`tagger_load` 6.1M, `disambig_rkyv` 4.1M, `disambig_apply` 0.2M,
`engine_analyze` 52K with the real artifacts — span invariants held).
