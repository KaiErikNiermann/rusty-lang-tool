//! Criterion benchmark: the native engine vs the nlprule baseline.
//!
//! Times `analyze` over a fixed prose corpus, `is_known` lexicon throughput, and cold load time. Each
//! group registers only the engines whose artifacts are present (so a fresh checkout still runs), so
//! the comparison appears only when both `cargo xtask build-tagger` and `fetch-engine` have run.
//!
//! `cargo bench -p rlt-native` (or `cargo xtask bench`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use rlt_core::Engine;
use rlt_engine::VendoredEngine;
use rlt_native::NativeEngine;
use std::hint::black_box;

/// A fixed multi-sentence prose fixture — representative load for `analyze` (segmentation,
/// tokenization, tagging, structural tags, disambiguation).
const CORPUS: &str = "The committee reviewed the proposal carefully before the meeting. \
Several members raised concerns about the budget, which had grown considerably. \
She argued that the new policy would affect thousands of employees across the country. \
In 2023, the organization reported record revenues of 4.2 million dollars. \
Nevertheless, the board decided to postpone its final decision until next quarter.";

/// Words for the `is_known` throughput bench — a mix of common, inflected, proper, and unknown forms.
const WORDS: &[&str] = &[
    "the", "running", "London", "quickly", "children", "recieve", "thoughtfulness", "an", "be",
    "zxqwv", "gives", "their", "should", "Paris", "establishments",
];

/// Resolve a workspace-root-relative path (the bench's CWD is the crate dir under `cargo bench -p`).
fn root(rel: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join(rel)
}

fn load_native() -> Option<NativeEngine> {
    let disambig = root("resources/disambig.rkyv");
    NativeEngine::from_paths(
        &root("resources/segment.srx"),
        &root("resources/tagger.rkyv"),
        disambig.exists().then_some(disambig.as_path()),
    )
    .ok()
}

fn load_nlprule() -> Option<VendoredEngine> {
    VendoredEngine::from_path(&root("resources/en_tokenizer.bin")).ok()
}

fn bench_analyze(c: &mut Criterion) {
    let mut group = c.benchmark_group("analyze");
    group.throughput(criterion::Throughput::Bytes(CORPUS.len() as u64));
    if let Some(engine) = load_native() {
        group.bench_function("native", |b| b.iter(|| black_box(engine.analyze(black_box(CORPUS)))));
    }
    if let Some(engine) = load_nlprule() {
        group.bench_function("nlprule", |b| b.iter(|| black_box(engine.analyze(black_box(CORPUS)))));
    }
    group.finish();
}

fn bench_is_known(c: &mut Criterion) {
    let mut group = c.benchmark_group("is_known");
    if let Some(engine) = load_native() {
        group.bench_function("native", |b| {
            b.iter(|| WORDS.iter().filter(|w| engine.is_known(black_box(w))).count());
        });
    }
    if let Some(engine) = load_nlprule() {
        group.bench_function("nlprule", |b| {
            b.iter(|| WORDS.iter().filter(|w| engine.is_known(black_box(w))).count());
        });
    }
    group.finish();
}

fn bench_load(c: &mut Criterion) {
    // Cold load reads + validates the multi-MB artifacts, so keep the sample small.
    let mut group = c.benchmark_group("load");
    group.sample_size(20).measurement_time(Duration::from_secs(10));
    if load_native().is_some() {
        group.bench_function("native", |b| b.iter(|| black_box(load_native())));
    }
    if load_nlprule().is_some() {
        group.bench_function("nlprule", |b| b.iter(|| black_box(load_nlprule())));
    }
    group.finish();
}

criterion_group!(benches, bench_analyze, bench_is_known, bench_load);
criterion_main!(benches);
