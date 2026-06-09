#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use rlt_core::{Analysis, GrammarChecker, IrMatcher};
use rlt_ir::Rule;

#[derive(Arbitrary, Debug)]
struct Input {
    rules: Vec<Rule>,
    analysis: Analysis,
    text: String,
}

// Compile the matcher from arbitrary IR rules (regex compilation, suggestion pre-compilation, the
// first-literal index) and run it over an arbitrary token graph + text. This stresses the
// backtracking matcher (min/max/skip recursion), antipattern suppression, marker-span resolution,
// and — the most panic-prone part — byte-offset suggestion rendering (`text.get(span)` and
// token-span slicing) against hostile spans that need not correspond to `text` at all.
fuzz_target!(|input: Input| {
    let matcher = IrMatcher::new(&input.rules);
    let _ = matcher.grammar_diagnostics(&input.text, &input.analysis);
});
