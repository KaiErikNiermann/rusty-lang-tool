<script lang="ts">
  import { onMount } from "svelte";
  import type * as Monaco from "monaco-editor";

  import { ArtifactStore, createArtifactStore } from "$lib/artifacts/store";
  import type { FetchState, WebManifest } from "$lib/artifacts/types";
  import { CheckerManager } from "$lib/checker/manager";
  import { ARTIFACT_BASE_URL, DEFAULT_LANG, MANIFEST_URL, SAMPLE_TEXT } from "$lib/config";
  import { registerRltCodeActions } from "$lib/editor/codeactions";
  import { DiagnosticIndex } from "$lib/editor/diagnostics";
  import { loadMonaco, RLT_THEME } from "$lib/editor/monaco";
  import DownloadProgress from "$lib/ui/DownloadProgress.svelte";
  import ErrorBanner from "$lib/ui/ErrorBanner.svelte";
  import LanguagePicker from "$lib/ui/LanguagePicker.svelte";

  let editorEl: HTMLDivElement;

  let manifest = $state<WebManifest | null>(null);
  let currentLang = $state(DEFAULT_LANG);
  let busy = $state(true);
  let ready = $state(false);
  let fetchState = $state<FetchState>({ kind: "idle" });
  let errorMessage = $state<string | null>(null);
  let findingCount = $state(0);

  // Non-reactive engine handles (set once in onMount).
  let monaco: typeof Monaco | undefined;
  let editor: Monaco.editor.IStandaloneCodeEditor | undefined;
  let model: Monaco.editor.ITextModel | undefined;
  let store: ArtifactStore | undefined;
  let manager: CheckerManager | undefined;
  const index = new DiagnosticIndex();
  let abort: AbortController | undefined;
  let debounceTimer: ReturnType<typeof setTimeout> | undefined;

  function runCheck() {
    if (!ready || !model || !manager || !monaco) return;
    const text = model.getValue();
    try {
      const diagnostics = manager.check(text);
      index.apply(monaco, model, text, diagnostics);
      findingCount = diagnostics.length;
    } catch (err) {
      console.error("check failed", err);
    }
  }

  function scheduleCheck() {
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(runCheck, 300);
  }

  async function selectLanguage(code: string) {
    if (!manager || !monaco || !model) return;
    abort?.abort();
    abort = new AbortController();
    busy = true;
    errorMessage = null;
    try {
      await manager.select(code, abort.signal);
      currentLang = code;
      ready = true;
      const sample = SAMPLE_TEXT[code];
      if (sample !== undefined) model.setValue(sample);
      runCheck();
    } catch (err) {
      if (err instanceof DOMException && err.name === "AbortError") return;
      errorMessage = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  onMount(() => {
    let disposed = false;
    let codeActions: Monaco.IDisposable | undefined;

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
      });
      model = editor.getModel() ?? undefined;
      codeActions = registerRltCodeActions(monaco, index);
      editor.onDidChangeModelContent(scheduleCheck);

      try {
        store = await createArtifactStore(MANIFEST_URL, ARTIFACT_BASE_URL);
        manifest = store.manifest;
        store.state.subscribe((s) => (fetchState = s));
        manager = new CheckerManager(store);
        await selectLanguage(DEFAULT_LANG);
      } catch (err) {
        errorMessage = err instanceof Error ? err.message : String(err);
        busy = false;
      }
    })();

    return () => {
      disposed = true;
      clearTimeout(debounceTimer);
      abort?.abort();
      codeActions?.dispose();
      manager?.dispose();
      editor?.dispose();
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

  <div class="overflow-hidden rounded-lg border border-white/10">
    <div bind:this={editorEl} class="h-[60vh] w-full"></div>
  </div>

  <footer class="flex items-center justify-between text-xs text-zinc-500">
    <span>
      {#if ready}
        {findingCount}
        {findingCount === 1 ? "issue" : "issues"} · hover a squiggle, or press
        <kbd class="rounded bg-white/10 px-1">Ctrl</kbd>+<kbd class="rounded bg-white/10 px-1">.</kbd>
        to fix
      {:else if !errorMessage}
        Loading the engine…
      {/if}
    </span>
    <span class="font-mono">L1 spelling · L2 grammar</span>
  </footer>
</main>
