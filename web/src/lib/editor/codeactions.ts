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

/**
 * Register quick-fixes: per marker, one "Replace with 'X'" action per suggestion (first preferred);
 * plus a single "Fix all" applying every current diagnostic's first suggestion in one workspace edit
 * (ranges are non-overlapping, so order is irrelevant). Returns a disposable.
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
        actions.push({
          title: `Fix all (${fixable.length})`,
          kind: "quickfix",
          edit: {
            edits: fixable.map(({ span, text }) => ({
              resource: model.uri,
              versionId: model.getVersionId(),
              textEdit: { range: byteSpanToRange(model, b2u, span), text },
            })),
          },
        });
      }

      return { actions, dispose() {} };
    },
  });
}
