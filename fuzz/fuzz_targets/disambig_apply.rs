#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use rlt_core::{Disambiguator, Token};
use rlt_ir::DisambigRule;

#[derive(Arbitrary, Debug)]
struct Input {
    rules: Vec<DisambigRule>,
    tokens: Vec<Token>,
}

// Compile the disambiguator from arbitrary rules (regex compilation, marker resolution) and run it
// over an arbitrary token graph. Stresses the reused backtracking matcher (min/max/skip recursion),
// non-overlapping match scanning, and the tag-action application (retain/push over tags & lemmas).
fuzz_target!(|input: Input| {
    let disambiguator = Disambiguator::new(&input.rules);
    let mut tokens = input.tokens;
    disambiguator.disambiguate(&mut tokens);
});
