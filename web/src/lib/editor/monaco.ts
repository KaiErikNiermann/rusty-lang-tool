// Monaco is browser-only and heavy — always loaded via `loadMonaco()` from `onMount`, never at module
// top level (keeps it out of SSR). One editor.worker is wired through Vite's `?worker` import so basic
// editing features work without the language-service workers we don't need (plain text + our markers).
import type * as Monaco from "monaco-editor";

import type { DiagnosticSource } from "../checker/types";

let monacoPromise: Promise<typeof Monaco> | null = null;

/** Per-source squiggle/overview colors — kept in sync with tailwind.config.ts tokens. */
export const SOURCE_COLOR: Record<DiagnosticSource, string> = {
  Spelling: "#f14c4c", // VS Code error red — brighter, so it reads on the gray editor background
  Grammar: "#3b9eff",
  Statistical: "#c586c0",
  Neural: "#4ec9b0",
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
        // VS Code Dark+ grey rather than near-black, so red error squiggles stand out (they were
        // hard to see on the previous very-dark background).
        "editor.background": "#1e1e1e",
        "editorGutter.background": "#1e1e1e",
        "editor.lineHighlightBackground": "#2a2d2e",
        // Squiggle palette: word-level errors (Spelling + L3 confusion → Warning) read red; structural
        // (Grammar + L4 → Info) read blue. Two crisp categories rather than Monaco's default amber/teal.
        "editorWarning.foreground": SOURCE_COLOR.Spelling,
        "editorWarning.border": "#00000000",
        "editorInfo.foreground": SOURCE_COLOR.Grammar,
        "editorInfo.border": "#00000000",
      },
    });
    return monaco;
  })();
  return monacoPromise;
}
