<script lang="ts">
  import { onMount } from "svelte";
  import type * as Monaco from "monaco-editor";

  import type { FetchState, WebManifest } from "$lib/artifacts/types";
  import { makeByteToUtf16 } from "$lib/checker/spanmap";
  import type { Diagnostic } from "$lib/checker/types";
  import { WorkerChecker } from "$lib/checker/worker-client";
  import { ARTIFACT_BASE_URL, DEFAULT_LANG, MANIFEST_URL, SAMPLE_TEXT } from "$lib/config";
  import { registerRltCodeActions } from "$lib/editor/codeactions";
  import { DiagnosticIndex } from "$lib/editor/diagnostics";
  import { applyFixAll, applyReplacement, byteSpanToRange } from "$lib/editor/fixes";
  import { loadMonaco, RLT_THEME } from "$lib/editor/monaco";
  import DownloadProgress from "$lib/ui/DownloadProgress.svelte";
  import ErrorBanner from "$lib/ui/ErrorBanner.svelte";
  import FindingsPanel from "$lib/ui/FindingsPanel.svelte";
  import LanguagePicker from "$lib/ui/LanguagePicker.svelte";

  let editorEl: HTMLDivElement;

  let manifest = $state<WebManifest | null>(null);
  let currentLang = $state(DEFAULT_LANG);
  let busy = $state(true);
  let ready = $state(false);
  let fetchState = $state<FetchState>({ kind: "idle" });
  let errorMessage = $state<string | null>(null);
  // The current check's findings plus the exact snapshot they were computed against. Byte spans are
  // only valid against `checkedText`, so the panel's apply path guards on `model.getValue() === checkedText`.
  let diagnostics = $state<Diagnostic[]>([]);
  let checkedText = $state("");
  const findingCount = $derived(diagnostics.length);

  // The cascade layers active for the current language — L3 is shown when it ships a confusion model.
  const activeLayers = $derived(
    manifest?.languages[currentLang]?.files["confusion.rkyv"]
      ? "L1 spelling · L2 grammar · L3 confusion"
      : "L1 spelling · L2 grammar",
  );

  // Non-reactive engine handles (set once in onMount).
  let monaco: typeof Monaco | undefined;
  let editor: Monaco.editor.IStandaloneCodeEditor | undefined;
  let model: Monaco.editor.ITextModel | undefined;
  let client: WorkerChecker | undefined;
  let overflowWidgets: HTMLDivElement | undefined;
  const index = new DiagnosticIndex();
  let debounceTimer: ReturnType<typeof setTimeout> | undefined;
  let checkSeq = 0;

  async function runCheck() {
    if (!ready || !model || !client || !monaco) return;
    const text = model.getValue();
    const seq = ++checkSeq;
    try {
      const result = await client.check(text);
      // Drop a superseded result, and never map a snapshot's spans onto an edited model.
      if (seq !== checkSeq || !model || model.getValue() !== text) return;
      index.apply(monaco, model, text, result);
      diagnostics = result;
      checkedText = text;
    } catch (err) {
      if (seq === checkSeq) console.error("check failed", err);
    }
  }

  function scheduleCheck() {
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => void runCheck(), 300);
  }

  /** Panel applies are only valid while the model still matches the snapshot the spans were built on. */
  function spansValid(): boolean {
    return !!model && model.getValue() === checkedText;
  }

  // After an edit lands the byte spans are stale, so re-check immediately instead of waiting out the
  // debounce armed by the content-change event — the panel then refreshes within one (fast) check.
  function recheckNow() {
    clearTimeout(debounceTimer);
    void runCheck();
  }

  function fixOne(diag: Diagnostic, replacement: string) {
    if (!model || !spansValid()) return;
    const range = byteSpanToRange(model, makeByteToUtf16(checkedText), diag.span);
    applyReplacement(model, range, replacement);
    recheckNow();
  }

  function fixAll() {
    if (!model || !spansValid()) return;
    applyFixAll(model, makeByteToUtf16(checkedText), diagnostics);
    recheckNow();
  }

  function reveal(diag: Diagnostic) {
    if (!model || !editor || !spansValid()) return;
    const range = byteSpanToRange(model, makeByteToUtf16(checkedText), diag.span);
    editor.revealRangeInCenter(range);
    editor.setSelection(range);
    editor.focus();
  }

  async function selectLanguage(code: string) {
    if (!client || !monaco || !model) return;
    const previous = currentLang;
    // Highlight the picked language immediately (the artifact swap can take a beat) and revert if it
    // fails — so the button feels responsive instead of waiting on the internal load.
    currentLang = code;
    busy = true;
    errorMessage = null;
    // Drop the previous language's findings so the panel doesn't show stale rows during the load.
    diagnostics = [];
    checkedText = "";
    try {
      await client.select(code);
      ready = true;
      const sample = SAMPLE_TEXT[code];
      if (sample !== undefined) model.setValue(sample);
      await runCheck();
    } catch (err) {
      currentLang = previous;
      errorMessage = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  onMount(() => {
    let disposed = false;
    let codeActions: Monaco.IDisposable | undefined;

    // Render hover / suggest / quick-fix widgets into a body-level node so the rounded editor frame's
    // `overflow-hidden` can't clip them (e.g. hovering an error on the first line). `fixedOverflowWidgets`
    // makes them position:fixed; the explicit dom node guarantees they live outside the clipped frame.
    overflowWidgets = document.createElement("div");
    overflowWidgets.className = "monaco-editor rlt-overflow-widgets";
    document.body.appendChild(overflowWidgets);

    (async () => {
      monaco = await loadMonaco();
      if (disposed) return;
      editor = monaco.editor.create(editorEl, {
        value: "",
        language: "plaintext",
        theme: RLT_THEME,
        automaticLayout: true,
        minimap: { enabled: false },
        fontFamily: "IBM Plex Mono, ui-monospace, monospace",
        fontSize: 15,
        lineNumbers: "on",
        wordWrap: "on",
        padding: { top: 16, bottom: 16 },
        scrollBeyondLastLine: false,
        renderLineHighlight: "none",
        overviewRulerLanes: 2,
        fixedOverflowWidgets: true,
        overflowWidgetsDomNode: overflowWidgets,
      });
      model = editor.getModel() ?? undefined;
      codeActions = registerRltCodeActions(monaco, index);
      editor.onDidChangeModelContent(scheduleCheck);

      try {
        client = new WorkerChecker();
        client.state.subscribe((s) => (fetchState = s));
        manifest = await client.init(MANIFEST_URL, ARTIFACT_BASE_URL);
        await selectLanguage(DEFAULT_LANG);
      } catch (err) {
        errorMessage = err instanceof Error ? err.message : String(err);
        busy = false;
      }
    })();

    return () => {
      disposed = true;
      clearTimeout(debounceTimer);
      codeActions?.dispose();
      client?.dispose();
      editor?.dispose();
      overflowWidgets?.remove();
    };
  });
</script>

<svelte:head>
  <title>rusty-lang-tool — local grammar & spell checker</title>
</svelte:head>

<main class="mx-auto flex min-h-full max-w-4xl flex-col gap-4 px-4 py-8 font-sans text-zinc-200">
  <header class="flex flex-col gap-1">
    <h1 class="text-xl font-semibold tracking-tight text-white">
      rusty-lang-tool
      <span class="ml-1 font-mono text-xs font-normal text-zinc-500">· runs entirely in your browser</span>
    </h1>
    <p class="text-sm text-zinc-400">
      Spelling + grammar checking with LanguageTool's rules, compiled to WebAssembly. No login, no
      servers, no telemetry — the language model is fetched once and cached locally.
    </p>
  </header>

  {#if manifest}
    <LanguagePicker {manifest} current={currentLang} {busy} onselect={selectLanguage} />
  {/if}

  <DownloadProgress state={fetchState} />

  {#if errorMessage}
    <ErrorBanner message={errorMessage} onretry={() => selectLanguage(currentLang)} />
  {/if}

  <div class="flex gap-3">
    <div class="relative flex-1 overflow-hidden rounded-lg border border-white/10">
      <div
        bind:this={editorEl}
        class="h-[60vh] w-full transition duration-200"
        class:blur-[2px]={busy}
        class:opacity-50={busy}
        class:pointer-events-none={busy}
      ></div>
      {#if busy}
        <div class="pointer-events-none absolute inset-0 flex items-center justify-center">
          <span
            class="flex items-center gap-2 rounded-md bg-black/60 px-3 py-1.5 text-xs text-zinc-200 backdrop-blur-sm"
          >
            <span
              class="h-3 w-3 animate-spin rounded-full border-2 border-white/30 border-t-white/90"
            ></span>
            Loading {manifest?.languages[currentLang]?.label ?? "language"}…
          </span>
        </div>
      {/if}
    </div>

    {#if ready}
      <FindingsPanel {diagnostics} onfix={fixOne} onfixall={fixAll} onreveal={reveal} />
    {/if}
  </div>

  <footer class="flex items-center justify-between text-xs text-zinc-500">
    <span>
      {#if ready}
        {findingCount}
        {findingCount === 1 ? "issue" : "issues"} · open the findings panel, or press
        <kbd class="rounded bg-white/10 px-1">Ctrl</kbd>+<kbd class="rounded bg-white/10 px-1">.</kbd>
        in the editor
      {:else if !errorMessage}
        Loading the engine…
      {/if}
    </span>
    <span class="font-mono">{activeLayers}</span>
  </footer>
</main>
