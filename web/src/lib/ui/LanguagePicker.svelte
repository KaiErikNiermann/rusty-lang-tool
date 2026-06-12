<script lang="ts">
  import type { WebManifest } from "$lib/artifacts/types";

  interface Props {
    manifest: WebManifest;
    current: string;
    busy: boolean;
    onselect: (code: string) => void;
  }
  let { manifest, current, busy, onselect }: Props = $props();

  const sizeLabel = (bytes: number) =>
    bytes >= 1e7 ? `${Math.round(bytes / 1e6)} MB` : `${(bytes / 1e6).toFixed(1)} MB`;

  // Lightest first — the snappy defaults lead.
  const langs = $derived(
    Object.entries(manifest.languages).sort((a, b) => a[1].totalBytes - b[1].totalBytes),
  );
</script>

<div class="flex flex-wrap gap-2">
  {#each langs as [code, lang] (code)}
    <button
      type="button"
      class="group flex items-center gap-2 rounded-md border px-3 py-1.5 text-sm transition
        {code === current
        ? 'border-grammar/70 bg-grammar/10 text-white'
        : 'border-white/10 bg-white/[0.03] text-zinc-300 hover:border-white/25 hover:text-white'}"
      disabled={busy && code !== current}
      onclick={() => onselect(code)}
    >
      <span class="font-medium">{lang.label}</span>
      <span class="font-mono text-[11px] text-zinc-500 group-hover:text-zinc-400">
        {sizeLabel(lang.totalBytes)}
      </span>
    </button>
  {/each}
</div>
