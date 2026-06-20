<script lang="ts">
  import type { TrackId } from "$lib/config";

  interface Props {
    current: TrackId;
    busy: boolean;
    onselect: (track: TrackId) => void;
  }
  let { current, busy, onselect }: Props = $props();

  const TRACKS: { id: TrackId; label: string; hint: string }[] = [
    { id: "reliable", label: "Reliable", hint: "gzip · loads all at once · the dependable default" },
    {
      id: "fast",
      label: "Fast",
      hint: "brotli (~32% smaller) · spelling appears first, grammar streams in · experimental",
    },
  ];
</script>

<div class="flex items-center gap-2 text-xs">
  <span class="font-mono text-[11px] text-zinc-500">download</span>
  <div class="inline-flex rounded-md border border-white/10 bg-white/[0.03] p-0.5" role="group">
    {#each TRACKS as track (track.id)}
      <button
        type="button"
        title={track.hint}
        aria-pressed={current === track.id}
        class="flex items-center gap-1 rounded px-2.5 py-1 transition
          {current === track.id
          ? 'bg-grammar/15 text-white'
          : 'text-zinc-400 hover:text-zinc-200'}"
        disabled={busy}
        onclick={() => onselect(track.id)}
      >
        {track.label}
        {#if track.id === "fast"}
          <span
            class="rounded-sm bg-statistical/20 px-1 text-[9px] font-semibold uppercase tracking-wide text-statistical"
          >
            beta
          </span>
        {/if}
      </button>
    {/each}
  </div>
</div>
