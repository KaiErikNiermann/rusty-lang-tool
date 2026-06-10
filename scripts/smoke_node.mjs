// Node smoke test for the WASM build — the "web-native works" proof.
//
// Loads the wasm-pack (nodejs target) module, constructs a checker from the nlprule binaries
// (passed as byte buffers, exactly as a browser would supply them), runs a check and asserts the
// expected diagnostics. Run after `cargo xtask build-wasm` (or `wasm-pack build crates/rlt-wasm
// --target nodejs --out-dir pkg`):  `node scripts/smoke_node.mjs`.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(import.meta.url);

// wasm-pack's nodejs target emits a CommonJS module.
const { RltChecker } = require(join(root, "crates/rlt-wasm/pkg/rlt_wasm.js"));

function load(name) {
  const path = join(root, "resources", name);
  try {
    return new Uint8Array(readFileSync(path));
  } catch {
    console.error(`SKIP: ${path} missing — run \`cargo xtask fetch-engine\` first.`);
    process.exit(0);
  }
}

const checker = new RltChecker(load("en_tokenizer.bin"), load("en_rules.bin"));

const input = "I should of went their yesterday. I recieve teh msg.";
const diagnostics = checker.check(input);

console.log(`input: ${JSON.stringify(input)}`);
console.log(`diagnostics (${diagnostics.length}):`);
for (const d of diagnostics) {
  const fix = d.suggestions?.[0]?.replacement;
  console.log(`  [${d.span.start}..${d.span.end}] ${d.source}/${d.code}` + (fix ? ` -> ${JSON.stringify(fix)}` : ""));
}

// Assertions: the pipeline must run in wasm and produce real findings.
if (!Array.isArray(diagnostics) || diagnostics.length === 0) {
  console.error("FAIL: expected at least one diagnostic");
  process.exit(1);
}
const hasGrammar = diagnostics.some((d) => d.source === "Grammar");
const hasSpelling = diagnostics.some((d) => d.source === "Spelling" && d.suggestions?.some((s) => s.replacement === "receive"));
if (!hasGrammar) {
  console.error('FAIL: expected an L2 grammar diagnostic (e.g. "should of"/"of" -> "have")');
  process.exit(1);
}
if (!hasSpelling) {
  console.error('FAIL: expected an L1 spelling fix "recieve" -> "receive"');
  process.exit(1);
}

console.log("OK: WASM checker ran L1 spelling + L2 grammar in Node.");

// --- Native engine (pure-Rust, no nlprule): if its artifacts are present, prove it runs in wasm. ---
function loadText(name) {
  try {
    return readFileSync(join(root, "resources", name), "utf8");
  } catch {
    return null;
  }
}
function loadOpt(name) {
  try {
    return new Uint8Array(readFileSync(join(root, "resources", name)));
  } catch {
    return null;
  }
}
const srx = loadText("segment.srx");
const taggerRkyv = loadOpt("tagger.rkyv");
const irBlob = loadOpt("en.rkyv");
if (srx && taggerRkyv && irBlob) {
  const native = RltChecker.with_native(srx, taggerRkyv, loadOpt("disambig.rkyv") ?? new Uint8Array(), irBlob);
  const nvInput = "I should of went their yesterday. I recieve teh msg.";
  const nvDiags = native.check(nvInput);
  console.log(`native input: ${JSON.stringify(nvInput)} -> ${nvDiags.length} diagnostics`);
  for (const d of nvDiags) {
    const fix = d.suggestions?.[0]?.replacement;
    console.log(`  [${d.span.start}..${d.span.end}] ${d.source}/${d.code}` + (fix ? ` -> ${JSON.stringify(fix)}` : ""));
  }
  const hasGrammar = nvDiags.some((d) => d.source === "Grammar");
  const hasSpelling = nvDiags.some((d) => d.source === "Spelling" && d.suggestions?.some((s) => s.replacement === "receive"));
  if (!hasGrammar) {
    console.error("FAIL: native — expected an L2 grammar diagnostic");
    process.exit(1);
  }
  if (!hasSpelling) {
    console.error('FAIL: native — expected an L1 spelling fix "recieve" -> "receive"');
    process.exit(1);
  }
  console.log("OK: pure-native WASM checker ran L1 spelling + L2 grammar in Node (no nlprule).");
} else {
  console.log("SKIP native: resources/{segment.srx,tagger.rkyv,en.rkyv} missing (cargo xtask build-tagger + build-blob).");
}

// --- L4 (optional): if the neural artifact is present, prove the int8 tagger runs in wasm too. ---
function loadL4(name) {
  try {
    return new Uint8Array(readFileSync(join(root, "resources/l4", name)));
  } catch {
    return null;
  }
}
const l4 = {
  model: loadL4("model.int8.onnx"),
  tokenizer: loadL4("tokenizer.json"),
  labels: loadL4("labels.json"),
  meta: loadL4("meta.json"),
  verb: loadL4("verb-form-vocab.txt") ?? new Uint8Array(),
};
if (l4.model && l4.tokenizer && l4.labels && l4.meta) {
  const neural = RltChecker.with_neural(
    load("en_tokenizer.bin"),
    load("en_rules.bin"),
    l4.model,
    l4.tokenizer,
    l4.labels,
    l4.meta,
    l4.verb,
  );
  const nInput = "She go to school every day .";
  const nDiags = neural.check(nInput);
  console.log(`L4 input: ${JSON.stringify(nInput)} -> ${nDiags.length} diagnostics`);
  for (const d of nDiags.filter((d) => d.source === "Neural")) {
    console.log(`  [${d.span.start}..${d.span.end}] Neural -> ${JSON.stringify(d.suggestions?.[0]?.replacement)}`);
  }
  const hasNeural = nDiags.some((d) => d.source === "Neural" && d.suggestions?.some((s) => s.replacement === "goes"));
  if (!hasNeural) {
    console.error('FAIL: expected an L4 neural fix "go" -> "goes"');
    process.exit(1);
  }
  console.log("OK: WASM checker ran L4 neural tagging in Node.");
} else {
  console.log("SKIP L4: resources/l4 artifact missing (run `cargo xtask build-l4`).");
}
