<script lang="ts">
  interface Props {
    message: string;
    onretry: () => void;
  }
  let { message, onretry }: Props = $props();
</script>

<div class="flex items-center justify-between gap-3 rounded-md border border-spelling/40 bg-spelling/10 px-3 py-2 text-sm">
  <span class="text-spelling">⚠ {message}</span>
  <button
    type="button"
    class="rounded border border-spelling/50 px-2 py-0.5 text-xs text-spelling hover:bg-spelling/20"
    onclick={onretry}
  >
    Retry
  </button>
</div>
