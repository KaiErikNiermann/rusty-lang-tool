import type * as Monaco from "monaco-editor";

import { makeByteToUtf16 } from "../checker/spanmap";
import { type DiagnosticIndex, MARKER_OWNER } from "./diagnostics";
import { computeFixAllEdits } from "./fixes";

function markerRange(m: Monaco.editor.IMarkerData): Monaco.IRange {
  return {
    startLineNumber: m.startLineNumber,
    startColumn: m.startColumn,
    endLineNumber: m.endLineNumber,
    endColumn: m.endColumn,
  };
}

/**
 * Register quick-fixes: per marker, one "Replace with 'X'" action per suggestion (first preferred);
 * plus a single "Fix all" applying every current diagnostic's first suggestion in one workspace edit.
 * The edit ranking / overlap filtering lives in `./fixes` so the HTML findings panel applies the exact
 * same edits as this lightbulb. Returns a disposable.
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
            // Just the word — "replace" is implied by the quick-fix menu, and Monaco truncates long
            // titles from the end with no tooltip, so the word is all that should occupy the space.
            title: `“${s.replacement}”`,
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

      // Fix-all: every indexed diagnostic's first suggestion, in one edit. Ranges are recomputed from
      // byte spans against the current model text (== the last checked snapshot when no edit has landed).
      const b2u = makeByteToUtf16(model.getValue());
      const edits = computeFixAllEdits(model, b2u, index.all());
      if (edits.length > 1) {
        actions.push({
          title: `Fix all (${edits.length})`,
          kind: "quickfix",
          edit: {
            edits: edits.map(({ range, text }) => ({
              resource: model.uri,
              versionId: model.getVersionId(),
              textEdit: { range, text },
            })),
          },
        });
      }

      return { actions, dispose() {} };
    },
  });
}
