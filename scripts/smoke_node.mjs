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
