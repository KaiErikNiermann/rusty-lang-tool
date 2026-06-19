<script lang="ts">
  import type { Diagnostic, DiagnosticSource } from "$lib/checker/types";

  interface Props {
    /** Current check's diagnostics (best-first suggestions per item). */
    diagnostics: Diagnostic[];
    /** Apply a single suggestion's replacement to the flagged span. */
    onfix: (diag: Diagnostic, replacement: string) => void;
    /** Apply every fixable diagnostic's first suggestion at once. */
    onfixall: () => void;
    /** Scroll the editor to a diagnostic and select its span. */
    onreveal: (diag: Diagnostic) => void;
  }
  let { diagnostics, onfix, onfixall, onreveal }: Props = $props();

  // One boolean drives both viewports: collapsed rail / hidden off-canvas (false) vs. expanded (true).
  // Default collapsed — the panel is opt-in so the editor leads.
  let open = $state(false);
  let panelEl: HTMLElement | undefined;

  // Source → palette token + UI label, matching the Monaco squiggle colors (tailwind.config.ts).
  const SOURCE_META: Record<DiagnosticSource, { dot: string; label: string }> = {
    Spelling: { dot: "bg-spelling", label: "spelling" },
    Grammar: { dot: "bg-grammar", label: "grammar" },
    Statistical: { dot: "bg-statistical", label: "confusion" },
    Neural: { dot: "bg-neural", label: "neural" },
  };

  // Cap chips per row so a word with many candidates can't blow out the layout; the lightbulb still
  // exposes the full list on desktop.
  const MAX_CHIPS = 6;

  const fixableCount = $derived(diagnostics.filter((d) => d.suggestions.length > 0).length);
  const isMobile = () => window.matchMedia("(max-width: 767px)").matches;

  // On mobile the drawer covers the editor, so revealing a span has to close it first; on desktop the
  // rail sits beside the prose and stays put.
  function reveal(diag: Diagnostic) {
    onreveal(diag);
    if (isMobile()) open = false;
  }

  // Swipe-right dismisses the open mobile drawer. It's the topmost layer once open, so there's no
  // conflict with Monaco's own touch handling (the reason we don't bind swipe-to-*open*).
  let touchStartX = 0;
  function onTouchStart(e: TouchEvent) {
    touchStartX = e.changedTouches[0]?.clientX ?? 0;
  }
  function onTouchEnd(e: TouchEvent) {
    if ((e.changedTouches[0]?.clientX ?? 0) - touchStartX > 60) open = false;
  }

  // Move focus into the drawer when it opens as an overlay (mobile), so it reads as a modal surface.
  $effect(() => {
    if (open && panelEl && isMobile()) panelEl.focus();
  });
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && open && (open = false)} />

<!-- Mobile: dim the editor behind the open drawer; tap to dismiss. -->
{#if open}
  <button
    type="button"
    class="fixed inset-0 z-20 bg-black/50 backdrop-blur-[1px] md:hidden"
    aria-label="Close findings"
    onclick={() => (open = false)}
  ></button>
{/if}

<!-- Mobile: edge tab to open the drawer (explicit tap target — swipe-to-open fights the editor). -->
{#if !open}
  <button
    type="button"
    class="fixed right-0 top-1/2 z-20 flex -translate-y-1/2 items-center gap-1 rounded-l-md border
      border-r-0 border-white/15 bg-zinc-900/95 py-3 pr-1.5 pl-2 text-xs font-medium text-zinc-200
      shadow-lg backdrop-blur md:hidden"
    aria-label="Show findings"
    onclick={() => (open = true)}
  >
    <span style="writing-mode:vertical-rl">
      {diagnostics.length}
      {diagnostics.length === 1 ? "issue" : "issues"}
    </span>
  </button>
{/if}

<aside
  bind:this={panelEl}
  tabindex="-1"
  aria-label="Findings"
  class="z-30 flex flex-col overflow-hidden bg-zinc-900/95 backdrop-blur outline-none
    fixed inset-y-0 right-0 w-[85%] max-w-sm border-l border-white/10 shadow-2xl
    transition-transform duration-300 {open ? 'translate-x-0' : 'translate-x-full'}
    md:static md:h-[60vh] md:max-w-none md:translate-x-0 md:rounded-lg md:border md:border-white/10
    md:shadow-none md:shrink-0 md:transition-[width] {open ? 'md:w-80' : 'md:w-12'}"
  ontouchstart={onTouchStart}
  ontouchend={onTouchEnd}
>
  <!-- Desktop collapsed: vertical rail (expand handle + live count). Hidden on mobile (the edge tab
       above plays this role) and once expanded. -->
  {#if !open}
    <button
      type="button"
      class="hidden h-full w-full flex-col items-center gap-2 py-3 text-zinc-400 hover:text-white md:flex"
      aria-label="Expand findings"
      aria-expanded="false"
      onclick={() => (open = true)}
    >
      <svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
        stroke-linecap="round" stroke-linejoin="round"><path d="M15 18l-6-6 6-6" /></svg>
      <span class="rounded-full bg-white/10 px-1.5 py-0.5 text-[11px] font-medium tabular-nums">
        {diagnostics.length}
      </span>
      <span class="text-xs tracking-wide text-zinc-500" style="writing-mode:vertical-rl">findings</span>
    </button>
  {/if}

  <!-- Expanded body (both viewports). -->
  <div class="h-full flex-col {open ? 'flex' : 'hidden'}">
    <header class="flex items-center justify-between gap-2 border-b border-white/10 px-3 py-2">
      <h2 class="text-sm font-medium text-zinc-300">
        {diagnostics.length}
        {diagnostics.length === 1 ? "finding" : "findings"}
      </h2>
      <div class="flex items-center gap-1.5">
        {#if fixableCount > 1}
          <button
            type="button"
            class="rounded-md border border-white/15 bg-white/[0.04] px-2.5 py-1 text-xs font-medium
              text-zinc-200 transition hover:border-white/30 hover:text-white active:scale-95"
            onclick={onfixall}
          >
            Fix all ({fixableCount})
          </button>
        {/if}
        <button
          type="button"
          class="rounded-md p-1 text-zinc-400 transition hover:bg-white/10 hover:text-white"
          aria-label="Collapse findings"
          onclick={() => (open = false)}
        >
          <svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
            stroke-linecap="round" stroke-linejoin="round"><path d="M9 18l6-6-6-6" /></svg>
        </button>
      </div>
    </header>

    {#if diagnostics.length === 0}
      <p class="px-3 py-4 text-sm text-zinc-500">No issues found.</p>
    {:else}
      <ul class="flex-1 divide-y divide-white/[0.06] overflow-y-auto">
        {#each diagnostics as diag, i (i)}
          {@const meta = SOURCE_META[diag.source]}
          <li class="flex flex-col gap-2 px-3 py-2.5">
            <button
              type="button"
              class="group flex items-start gap-2 text-left"
              title="Show in editor"
              onclick={() => reveal(diag)}
            >
              <span class="mt-1.5 h-2 w-2 shrink-0 rounded-full {meta.dot}"></span>
              <span class="flex flex-col gap-0.5">
                <span class="text-sm text-zinc-200 group-hover:text-white">{diag.message}</span>
                <span class="font-mono text-[11px] uppercase tracking-wide text-zinc-500">
                  {meta.label}
                </span>
              </span>
            </button>

            {#if diag.suggestions.length > 0}
              <div class="flex flex-wrap gap-1.5 pl-4">
                {#each diag.suggestions.slice(0, MAX_CHIPS) as s, j (j)}
                  <button
                    type="button"
                    class="rounded-md border px-2.5 py-1 text-sm transition active:scale-95
                      {j === 0
                      ? 'border-grammar/50 bg-grammar/10 text-white hover:border-grammar/80'
                      : 'border-white/10 bg-white/[0.04] text-zinc-200 hover:border-white/30 hover:text-white'}"
                    onclick={() => onfix(diag, s.replacement)}
                  >
                    {s.replacement}
                  </button>
                {/each}
                {#if diag.suggestions.length > MAX_CHIPS}
                  <span class="self-center text-[11px] text-zinc-500">
                    +{diag.suggestions.length - MAX_CHIPS} more
                  </span>
                {/if}
              </div>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  </div>
</aside>
