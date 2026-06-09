# Licensing

This project deliberately keeps **code** and **data** licenses separate.

## Code — `Apache-2.0 OR MIT`

All Rust source in this repository is dual-licensed under Apache-2.0 or MIT, at your option
(see `LICENSE-APACHE` and `LICENSE-MIT`). This matches the licensing of nlprule, whose engine is
vendored (M2) behind the `rlt-core::Engine` trait.

## Data — `LGPL-2.1` (LanguageTool)

The rule corpus, confusion sets, and morphology dictionaries come from
[LanguageTool](https://github.com/languagetool-org/languagetool), licensed **LGPL-2.1**. They are
**not committed** to this repository; they are fetched on demand (`cargo xtask fetch-lt`).

Any build artifact *derived* from that data — the compiled `resources/en.rkyv` rule/dictionary
blob — is therefore a derivative of LGPL-2.1 data and must be distributed under LGPL-2.1 terms.

### What this means in practice

- The **engine and tooling** (this repo's code) are permissively licensed and reusable.
- The **shipped data blob** is LGPL-2.1. Keep it as a separately-distributed artifact, not baked
  into a binary that would relicense it. Ship the blob alongside the code, loaded at runtime.
- This is the same posture LanguageTool itself uses, and is what makes a free, fully-local
  reuse of LT's open rules legitimate — only LT's *cloud service, neural models, and server-side
  n-grams* are gated, none of which we depend on.
