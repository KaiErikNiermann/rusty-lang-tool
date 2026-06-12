<script lang="ts">
  import { onMount } from "svelte";
  import type * as Monaco from "monaco-editor";

  import type { FetchState, WebManifest } from "$lib/artifacts/types";
  import { WorkerChecker } from "$lib/checker/worker-client";
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
  const index = new DiagnosticIndex();
  let debounceTimer: ReturnType<typeof setTimeout> | undefined;
  let checkSeq = 0;

  async function runCheck() {
    if (!ready || !model || !client || !monaco) return;
    const text = model.getValue();
    const seq = ++checkSeq;
    try {
      const diagnostics = await client.check(text);
      // Drop a superseded result, and never map a snapshot's spans onto an edited model.
      if (seq !== checkSeq || !model || model.getValue() !== text) return;
      index.apply(monaco, model, text, diagnostics);
      findingCount = diagnostics.length;
    } catch (err) {
      if (seq === checkSeq) console.error("check failed", err);
    }
  }

  function scheduleCheck() {
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => void runCheck(), 300);
  }

  async function selectLanguage(code: string) {
    if (!client || !monaco || !model) return;
    busy = true;
    errorMessage = null;
    try {
      await client.select(code);
      currentLang = code;
      ready = true;
      const sample = SAMPLE_TEXT[code];
      if (sample !== undefined) model.setValue(sample);
      await runCheck();
    } catch (err) {
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
    <span class="font-mono">{activeLayers}</span>
  </footer>
</main>
