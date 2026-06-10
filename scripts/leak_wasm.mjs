// WASM leak probe — run the pure-native checker thousands of times and watch memory.
//
// A leak in the Rust/wasm side (allocations not freed on drop) shows as monotonic growth in the wasm
// linear memory / Node `arrayBuffers` that never plateaus. A healthy pipeline grows during warm-up
// (memory.grow) then flattens. Run after `cargo xtask build-wasm`:
//   node --expose-gc scripts/leak_wasm.mjs

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(import.meta.url);
const { RltChecker } = require(join(root, "crates/rlt-wasm/pkg/rlt_wasm.js"));

function bytes(name) {
  return new Uint8Array(readFileSync(join(root, "resources", name)));
}
function text(name) {
  return readFileSync(join(root, "resources", name), "utf8");
}

let srx, tagger, ir, disambig;
try {
  srx = text("segment.srx");
  tagger = bytes("tagger.rkyv");
  ir = bytes("en.rkyv");
  disambig = (() => {
    try {
      return bytes("disambig.rkyv");
    } catch {
      return new Uint8Array();
    }
  })();
} catch (e) {
  console.error(`SKIP: native artifacts missing (${e.message}). Run cargo xtask build-tagger + build-blob.`);
  process.exit(0);
}

const checker = RltChecker.with_native(srx, tagger, disambig, ir);
const INPUT = "I should of went their yesterday. She go to school. In 2023, they recieve teh msg.";

function sample() {
  if (global.gc) global.gc();
  const m = process.memoryUsage();
  return { rss: m.rss, external: m.external, arrayBuffers: m.arrayBuffers };
}
const mb = (n) => (n / 1048576).toFixed(2);

// Warm up (lets the wasm allocator reach steady state), then sample across batches.
for (let i = 0; i < 200; i++) checker.check(INPUT);
const base = sample();
console.log(`baseline: rss=${mb(base.rss)}MB external=${mb(base.external)}MB arrayBuffers=${mb(base.arrayBuffers)}MB`);

const BATCHES = 10;
const PER_BATCH = 2000;
let last = base;
for (let b = 1; b <= BATCHES; b++) {
  for (let i = 0; i < PER_BATCH; i++) checker.check(INPUT);
  const s = sample();
  console.log(
    `after ${b * PER_BATCH} checks: rss=${mb(s.rss)}MB (Δ${mb(s.rss - last.rss)}) ` +
      `external=${mb(s.external)}MB (Δ${mb(s.external - last.external)}) ` +
      `arrayBuffers=${mb(s.arrayBuffers)}MB`,
  );
  last = s;
}

// Verdict: growth from the post-warmup baseline over 20k checks should be bounded (the wasm linear
// memory is reused, not leaked). Allow generous slack for allocator headroom / V8 noise.
const grewExternalMB = (last.external - base.external) / 1048576;
const grewRssMB = (last.rss - base.rss) / 1048576;
console.log(`\ntotal growth over ${BATCHES * PER_BATCH} checks: external=${grewExternalMB.toFixed(2)}MB rss=${grewRssMB.toFixed(2)}MB`);
if (grewExternalMB > 16) {
  console.error(`FAIL: wasm external memory grew ${grewExternalMB.toFixed(2)}MB — looks like a leak.`);
  process.exit(1);
}
console.log("OK: wasm memory growth bounded across 20k native checks (no unbounded leak).");
