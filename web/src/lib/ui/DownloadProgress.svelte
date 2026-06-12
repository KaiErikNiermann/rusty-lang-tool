<script lang="ts">
  import type { FetchState } from "$lib/artifacts/types";

  interface Props {
    state: FetchState;
  }
  let { state }: Props = $props();

  const mb = (b: number) => (b / 1e6).toFixed(1);
</script>

{#if state.kind === "downloading" || state.kind === "verifying"}
  <div class="flex flex-col gap-1.5">
    <div class="flex justify-between text-xs text-zinc-400">
      {#if state.kind === "downloading"}
        <span>Downloading {state.file} artifacts…</span>
        <span class="font-mono">{mb(state.loaded)} / {mb(state.total)} MB</span>
      {:else}
        <span>Verifying {state.file}…</span>
      {/if}
    </div>
    <div class="h-1.5 w-full overflow-hidden rounded-full bg-white/10">
      <div
        class="h-full rounded-full bg-grammar transition-[width] duration-150"
        style:width={state.kind === "downloading" ? `${state.pct}%` : "100%"}
        class:animate-pulse={state.kind === "verifying"}
      ></div>
    </div>
  </div>
{/if}
