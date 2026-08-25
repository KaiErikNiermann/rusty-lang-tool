# `.semgrep/` — repo-specific rules

Two rule files, matching the split used in `lang-check`:

| File | Question it answers |
|---|---|
| `conventions.yml` | Boundaries this codebase holds that a general linter cannot know. |
| `core-utils.yml`  | "We already generalized this — don't grow another private copy." |

Run them:

```sh
just semgrep          # or: poetry run semgrep --config .semgrep/ --error .
```

`semgrep` is pinned in `pyproject.toml` (Poetry, Python 3.12) so CI, the pre-push hook and every
developer run the same version — rule syntax and defaults drift between releases.

## The discipline

Every rule records, in its own message, the count it found when it was written. That is the point:
a pattern rule matching nothing is indistinguishable from a clean codebase, so each rule has a
documented answer it must find. A rule that is deliberately forward-only says **FORWARD GUARD** in
its message rather than implying a backlog it does not have.

Two sections at the bottom of `conventions.yml` record rules **deliberately absent** (a non-test
`.unwrap()` ban; a per-crate `#![forbid(unsafe_code)]` requirement) with the measurements that
disqualified them. A rule whose every current hit is correct code teaches people to skim the linter.

## What semgrep here does *not* cover

- **`.svelte` files.** Semgrep cannot parse them at all — it scans zero files, silently. Since
  `routes/+page.svelte` is where the editor is actually wired up, `web/` carries its own ESLint
  (`web/eslint.config.js`, `pnpm lint`) with sonarjs / unicorn / security. Anything asserted about
  a component has to be asserted there.
- **Anything cross-file.** Semgrep matches patterns within a single file. It has no notion of
  "for each X in this list, assert Y exists elsewhere" — see below.

## Partial languages: why this is *not* mostly a semgrep problem

Adding a language touches many files, so "did I finish?" is a real question. It is already answered,
by three mechanisms, and semgrep is the weakest of them:

1. **The compiler**, for completeness of the config itself. `LangConfig` derives only
   `Debug, Clone, Copy` — **no `Default`** — so a `LangConfig { .. }` literal missing any field is a
   compile error. There is nothing for a linter to add: every language necessarily specifies every
   field.

2. **`cargo xtask lang-coherence`**, for cross-file wiring. It iterates the canonical
   `rlt_lang::LANGUAGES` and checks each language is present at every site that cannot derive from
   that list — `lang-manifests/<code>.json` exists, `SPARSE_PATHS` contains its sparse path, a
   `reads_real_languagetool_<name>_dict` test exists, a `<code>_native_reproduces_examples` oracle
   test exists, and (for L3 languages) a `confusion_corpus` arm exists. Required failures gate CI;
   recommended ones warn.

   This is set-difference against a runtime list plus filesystem checks. Semgrep can do neither, so
   for this class of check it is not a weaker option — it is not an option.

   It also covers the one failure nothing else could see: a `pub static X: LangConfig` that was
   never appended to `LANGUAGES`. That is silent — a `pub` item in a library crate is reachable API
   and so never trips `dead_code`, `config()`/`known()` derive from the list so the language simply
   does not exist at runtime, and the per-language loop cannot see it because it iterates the very
   list the language is missing from. It is checked by reading the source text, because Rust has no
   reflection: no test inside the crate can enumerate statics that nothing references.

3. **Semgrep**, for the one thing it is actually good at here: `langconfig-built-outside-rlt-lang`
   catches a `LangConfig` constructed anywhere other than `rlt-lang`, which would be a language that
   `LANGUAGES`, `--help`, the CI matrix and the coherence report all disagree about. That is a
   single-file syntactic shape, which is semgrep's home ground.

**So: both, with a clear division.** Structural/enumerable invariants belong in `lang-coherence`
(and stay there — it is the tool that can express them); syntactic "never write this shape"
invariants belong here. Do not try to reimplement (2) as semgrep rules; it cannot express them and
the result would be a rule that looks like a check and is not.

## Gotchas found while writing these

- **Struct-literal patterns do not parse.** `LangConfig { ... }` is a hard parse error for
  semgrep's Rust grammar in every expression context tried. `langconfig-built-outside-rlt-lang`
  uses a regex, anchored on a preceding `=`/`(`/`,` so it does not match `fn f() -> LangConfig {`.
- **`a::b(x, y)` and `x.b(y)` normalize to the same AST.** `$S.ceil_char_boundary(...)` therefore
  matches `rlt_core::ceil_char_boundary(s, i)` — the *fix* — so `share-char-boundary-snapping`
  needs explicit `pattern-not`s for the shared helpers.
- **`#[cfg(test)]` does not bind to the item** for `pattern-not-inside`. Anchor on `mod tests { ... }`
  instead; every test module in this workspace is named `tests`, checked.
- **`tests/` is in semgrep's built-in ignore list.** `.semgrepignore` re-admits it, because
  `crates/rlt-cli/tests/` is the differential oracle and handles engine byte offsets exactly like
  production does.
- **Bare relative path globs are being re-anchored** by Semgrepignore v2. Paths here are written
  `/crates/...` (anchored) or `**/tests/` (explicitly unanchored) to avoid the deprecation.
