// Monaco is browser-only and heavy — always loaded via `loadMonaco()` from `onMount`, never at module
// top level (keeps it out of SSR). One editor.worker is wired through Vite's `?worker` import so basic
// editing features work without the language-service workers we don't need (plain text + our markers).
import type * as Monaco from "monaco-editor";

import type { DiagnosticSource } from "../checker/types";

let monacoPromise: Promise<typeof Monaco> | null = null;

/** Per-source squiggle/overview colors — kept in sync with tailwind.config.ts tokens. */
export const SOURCE_COLOR: Record<DiagnosticSource, string> = {
  Spelling: "#e5484d",
  Grammar: "#3b82f6",
  Statistical: "#a855f7",
  Neural: "#10b981",
};

/** The dark "techy" theme; minimal, so the editor reads as a tool, not a document. */
export const RLT_THEME = "rlt-dark";

/** Lazy-load Monaco, register the worker + theme exactly once. */
export async function loadMonaco(): Promise<typeof Monaco> {
  monacoPromise ??= (async () => {
    const monaco = await import("monaco-editor");
    const { default: EditorWorker } = await import(
      "monaco-editor/esm/vs/editor/editor.worker?worker"
    );
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (self as unknown as { MonacoEnvironment: Monaco.Environment }).MonacoEnvironment = {
      getWorker: () => new EditorWorker(),
    };
    monaco.editor.defineTheme(RLT_THEME, {
      base: "vs-dark",
      inherit: true,
      rules: [],
      colors: {
        "editor.background": "#0f1115",
        "editorGutter.background": "#0f1115",
        "editor.lineHighlightBackground": "#161a21",
      },
    });
    return monaco;
  })();
  return monacoPromise;
}
