// The shape `RltChecker.check()` returns (serialized from rlt_core::Diagnostic via serde-wasm-bindgen).

/** Half-open byte range [start, end) into the checked UTF-8 text. */
export interface Span {
  start: number;
  end: number;
}

/** A single proposed replacement for a diagnostic's span. */
export interface Suggestion {
  replacement: string;
}

/** Which cascade layer produced the diagnostic. */
export type DiagnosticSource = "Spelling" | "Grammar" | "Statistical" | "Neural";

/** One detected issue. `code` is a rule id (`A_INFINITIVE`) or `SPELL`; suggestions are best-first. */
export interface Diagnostic {
  span: Span;
  code: string;
  message: string;
  suggestions: Suggestion[];
  source: DiagnosticSource;
}
