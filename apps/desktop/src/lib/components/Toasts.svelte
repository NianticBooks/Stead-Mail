<script lang="ts">
  import { app } from "../state.svelte";
  import Icon from "./Icon.svelte";
</script>

<div class="toasts" aria-live="polite">
  {#each app.toasts as t (t.id)}
    <div class="toast {t.kind}" role={t.kind === "error" ? "alert" : "status"}>
      <Icon name={t.kind === "error" ? "alert" : t.kind === "success" ? "check" : "mail"} size={16} />
      <span class="text">{t.text}</span>
      {#if t.action}<button class="btn sm ghost" onclick={() => { t.action?.run(); app.dismissToast(t.id); }}>{t.action.label}</button>{/if}
      <button class="icon-btn" onclick={() => app.dismissToast(t.id)} aria-label="Dismiss"><Icon name="x" size={14} /></button>
    </div>
  {/each}
</div>

<style>
  .toasts { position: fixed; right: 16px; bottom: 16px; display: grid; gap: 8px; z-index: 100; max-width: 420px; }
  .toast {
    display: flex; align-items: center; gap: 10px; padding: 9px 8px 9px 12px; background: var(--bg-3);
    border: 1px solid var(--line-strong); border-left-width: 3px; border-radius: var(--radius); box-shadow: var(--shadow);
  }
  .toast.error { border-left-color: var(--danger); }
  .toast.error :global(svg) { color: var(--danger); }
  .toast.success { border-left-color: var(--ok); }
  .toast.success :global(svg) { color: var(--ok); }
  .toast.info { border-left-color: var(--accent); }
  .text { flex: 1; overflow-wrap: anywhere; }
</style>
