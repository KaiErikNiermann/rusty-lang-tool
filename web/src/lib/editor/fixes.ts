import type * as Monaco from "monaco-editor";

import type { Diagnostic, Span } from "../checker/types";

/** UTF-16 editor range for a byte `span`, using a byte→UTF-16 mapper built for the matching snapshot. */
export function byteSpanToRange(
  model: Monaco.editor.ITextModel,
  b2u: (byteOffset: number) => number,
  span: Span,
): Monaco.IRange {
  const start = model.getPositionAt(b2u(span.start));
  const end = model.getPositionAt(b2u(span.end));
  return {
    startLineNumber: start.lineNumber,
    startColumn: start.column,
    endLineNumber: end.lineNumber,
    endColumn: end.column,
  };
}

/** True if range `a` ends at or before (line, col) — i.e. it does not reach into that position. */
function before(a: Monaco.IRange, bStartLine: number, bStartCol: number): boolean {
  return a.endLineNumber < bStartLine || (a.endLineNumber === bStartLine && a.endColumn <= bStartCol);
}

export interface RangedEdit {
  range: Monaco.IRange;
  text: string;
}

/**
 * Rank every diagnostic's first suggestion into a document-ordered, non-overlapping edit list.
 * Two rules can flag the same or adjacent span (e.g. a spacing rule pair); applying both would make
 * the edit ranges overlap, which Monaco rejects ("Overlapping ranges are not allowed"). So the edits
 * are sorted by position and greedily filtered — the first edit in document order wins a conflict.
 */
export function computeFixAllEdits(
  model: Monaco.editor.ITextModel,
  b2u: (byteOffset: number) => number,
  diagnostics: Diagnostic[],
): RangedEdit[] {
  const ranked = diagnostics
    .flatMap((d) => {
      const first = d.suggestions[0];
      return first ? [{ range: byteSpanToRange(model, b2u, d.span), text: first.replacement }] : [];
    })
    .sort(
      (a, b) =>
        a.range.startLineNumber - b.range.startLineNumber ||
        a.range.startColumn - b.range.startColumn,
    );

  const nonOverlapping: RangedEdit[] = [];
  for (const e of ranked) {
    const prev = nonOverlapping.at(-1);
    if (!prev || before(prev.range, e.range.startLineNumber, e.range.startColumn)) {
      nonOverlapping.push(e);
    }
  }
  return nonOverlapping;
}

/** Imperatively apply one replacement as an undoable edit (the HTML findings panel's apply path). */
export function applyReplacement(
  model: Monaco.editor.ITextModel,
  range: Monaco.IRange,
  text: string,
): void {
  model.pushEditOperations([], [{ range, text }], () => null);
}

/**
 * Imperatively apply every diagnostic's first suggestion in a single undoable edit. Returns how many
 * edits were applied (after overlap filtering) so the caller can no-op on an empty set.
 */
export function applyFixAll(
  model: Monaco.editor.ITextModel,
  b2u: (byteOffset: number) => number,
  diagnostics: Diagnostic[],
): number {
  const edits = computeFixAllEdits(model, b2u, diagnostics);
  if (edits.length > 0) {
    model.pushEditOperations(
      [],
      edits.map((e) => ({ range: e.range, text: e.text })),
      () => null,
    );
  }
  return edits.length;
}
