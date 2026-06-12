<script lang="ts">
  import "../app.css";

  let { children } = $props();
</script>

<div class="min-h-full bg-[#0f1115] text-zinc-200 antialiased">
  {@render children()}
</div>
