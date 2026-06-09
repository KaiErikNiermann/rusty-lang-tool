#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use rlt_core::{Analysis, ConfusionChecker, GrammarChecker};
use rlt_ir::ConfusionModel;

#[derive(Arbitrary, Debug)]
struct Input {
    model: ConfusionModel,
    analysis: Analysis,
    text: String,
}

// Build the L3 confusion checker from an arbitrary model and run it over an arbitrary token graph,
// stressing the bigram / POS-context log-ratio lookups, neighbour selection, contraction-head and
// evidence guards, and the recase + span emission on the suggested correction.
fuzz_target!(|input: Input| {
    let checker = ConfusionChecker::new(&input.model);
    let _ = checker.grammar_diagnostics(&input.text, &input.analysis);
});
