import type * as Monaco from "monaco-editor";

import { makeByteToUtf16 } from "../checker/spanmap";
import type { Span } from "../checker/types";
import { type DiagnosticIndex, MARKER_OWNER } from "./diagnostics";

function markerRange(m: Monaco.editor.IMarkerData): Monaco.IRange {
  return {
    startLineNumber: m.startLineNumber,
    startColumn: m.startColumn,
    endLineNumber: m.endLineNumber,
    endColumn: m.endColumn,
  };
}

function byteSpanToRange(
  model: Monaco.editor.ITextModel,
  b2u: (b: number) => number,
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

/** True if `a` is strictly before `b` (a's end column/line does not reach b's start). */
function before(a: Monaco.IRange, bStartLine: number, bStartCol: number): boolean {
  return a.endLineNumber < bStartLine || (a.endLineNumber === bStartLine && a.endColumn <= bStartCol);
}

/**
 * Register quick-fixes: per marker, one "Replace with 'X'" action per suggestion (first preferred);
 * plus a single "Fix all" applying every current diagnostic's first suggestion in one workspace edit.
 * Two rules can flag the same or adjacent span (e.g. a spacing rule pair), which would make the
 * Fix-all edit list overlap — Monaco rejects that with "Overlapping ranges are not allowed". So the
 * edits are sorted and greedily filtered to a non-overlapping set. Returns a disposable.
 */
export function registerRltCodeActions(monaco: typeof Monaco, index: DiagnosticIndex): Monaco.IDisposable {
  return monaco.languages.registerCodeActionProvider("plaintext", {
    provideCodeActions(model, _range, context) {
      const actions: Monaco.languages.CodeAction[] = [];

      for (const marker of context.markers) {
        if (marker.source !== MARKER_OWNER) continue;
        const diag = index.forMarker(marker);
        if (!diag) continue;
        diag.suggestions.forEach((s, i) => {
          actions.push({
            title: `Replace with “${s.replacement}”`,
            kind: "quickfix",
            diagnostics: [marker],
            isPreferred: i === 0,
            edit: {
              edits: [
                {
                  resource: model.uri,
                  versionId: model.getVersionId(),
                  textEdit: { range: markerRange(marker), text: s.replacement },
                },
              ],
            },
          });
        });
      }

      // Fix-all: every indexed diagnostic with a suggestion, in one edit. Ranges recomputed from byte
      // spans against the current model text (== the last checked snapshot when no edit has landed).
      const fixable = index.all().flatMap((d) => {
        const first = d.suggestions[0];
        return first ? [{ span: d.span, text: first.replacement }] : [];
      });
      if (fixable.length > 1) {
        const b2u = makeByteToUtf16(model.getValue());
        const ranked = fixable
          .map(({ span, text }) => ({ range: byteSpanToRange(model, b2u, span), text }))
          .sort(
            (a, b) =>
              a.range.startLineNumber - b.range.startLineNumber ||
              a.range.startColumn - b.range.startColumn,
          );
        // Greedily keep edits whose range starts at/after the previous kept edit's end (drop overlaps).
        const nonOverlapping: typeof ranked = [];
        for (const e of ranked) {
          const prev = nonOverlapping.at(-1);
          if (!prev || before(prev.range, e.range.startLineNumber, e.range.startColumn)) {
            nonOverlapping.push(e);
          }
        }
        if (nonOverlapping.length > 1) {
          actions.push({
            title: `Fix all (${nonOverlapping.length})`,
            kind: "quickfix",
            edit: {
              edits: nonOverlapping.map(({ range, text }) => ({
                resource: model.uri,
                versionId: model.getVersionId(),
                textEdit: { range, text },
              })),
            },
          });
        }
      }

      return { actions, dispose() {} };
    },
  });
}
