import type * as Monaco from "monaco-editor";

import { makeByteToUtf16 } from "../checker/spanmap";
import type { Diagnostic, DiagnosticSource } from "../checker/types";

/** Owner string for our markers, so a re-check fully replaces the previous set. */
export const MARKER_OWNER = "rlt";

function severityFor(monaco: typeof Monaco, source: DiagnosticSource): Monaco.MarkerSeverity {
  switch (source) {
    case "Spelling":
    case "Statistical": // L3 real-word errors are as actionable as spelling
      return monaco.MarkerSeverity.Warning;
    case "Grammar":
    case "Neural":
      return monaco.MarkerSeverity.Info;
    default:
      return monaco.MarkerSeverity.Hint;
  }
}

const rangeKey = (m: { startLineNumber: number; startColumn: number; endLineNumber: number; endColumn: number }) =>
  `${m.startLineNumber}:${m.startColumn}:${m.endLineNumber}:${m.endColumn}`;

/**
 * Holds the current check's diagnostics indexed by editor range, so the code-action provider can map a
 * marker back to its suggestions. Rebuilt on every check; markers carry no payload themselves.
 */
export class DiagnosticIndex {
  private byRange = new Map<string, Diagnostic>();

  /**
   * Map `diagnostics` (byte spans over `text`) to Monaco markers, set them on `model`, and refresh the
   * range→diagnostic index. `text` must be the exact snapshot passed to the checker.
   */
  apply(monaco: typeof Monaco, model: Monaco.editor.ITextModel, text: string, diagnostics: Diagnostic[]): void {
    const b2u = makeByteToUtf16(text);
    this.byRange.clear();
    const markers: Monaco.editor.IMarkerData[] = diagnostics.map((d) => {
      const start = model.getPositionAt(b2u(d.span.start));
      const end = model.getPositionAt(b2u(d.span.end));
      const marker: Monaco.editor.IMarkerData = {
        startLineNumber: start.lineNumber,
        startColumn: start.column,
        endLineNumber: end.lineNumber,
        endColumn: end.column,
        message: d.message || d.code,
        severity: severityFor(monaco, d.source),
        code: `${d.source}/${d.code}`,
        source: MARKER_OWNER,
      };
      this.byRange.set(rangeKey(marker), d);
      return marker;
    });
    monaco.editor.setModelMarkers(model, MARKER_OWNER, markers);
  }

  /** The diagnostic whose range exactly matches `marker`, if any. */
  forMarker(marker: Monaco.editor.IMarkerData): Diagnostic | undefined {
    return this.byRange.get(rangeKey(marker));
  }

  /** Every (range, diagnostic) currently indexed — used to build "Fix all". */
  all(): Diagnostic[] {
    return [...this.byRange.values()];
  }

  clear(monaco: typeof Monaco, model: Monaco.editor.ITextModel): void {
    this.byRange.clear();
    monaco.editor.setModelMarkers(model, MARKER_OWNER, []);
  }
}
