<script lang="ts">
  import { autofocus } from "../focus";
  import { cal } from "../calendar.svelte";
  import Icon from "./Icon.svelte";

  const when = (iso: string) => {
    const d = new Date(iso);
    return Number.isNaN(d.getTime()) ? iso : d.toLocaleString(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
  };

  const label: Record<string, string> = { conflict: "Conflict", error: "Error", info: "Note", deleted: "Removed" };

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      cal.logOpen = false;
    }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div class="overlay" role="dialog" aria-modal="true" aria-label="Sync log" tabindex="-1" use:autofocus={"button.close"} {onkeydown}>
  <div class="modal">
    <header>
      <h2>Sync log</h2>
      <span class="spacer"></span>
      <button class="icon-btn close" onclick={() => (cal.logOpen = false)} aria-label="Close"><Icon name="x" /></button>
    </header>
    <div class="body">
      {#if cal.log.length === 0}
        <p class="muted">Nothing to report. When a change here and a change on the server collide, what was kept and what was lost is recorded here.</p>
      {/if}
      {#each cal.log as e (e.id)}
        <article class={e.kind}>
          <div class="head">
            <span class="tag">{label[e.kind] ?? e.kind}</span>
            <strong>{e.title || e.calendar}</strong>
            <span class="muted small">{e.title ? e.calendar + " · " : ""}{when(e.at)}</span>
          </div>
          <div class="detail">{e.detail}</div>
        </article>
      {/each}
    </div>
    <footer>
      <button class="btn sm ghost" onclick={() => cal.clearLog()} disabled={cal.log.length === 0}><Icon name="trash" size={13} /> Clear log</button>
      <span class="spacer"></span>
      <button class="btn primary" onclick={() => (cal.logOpen = false)}>Done</button>
    </footer>
  </div>
</div>

<style>
  .modal { width: min(600px, 100%); max-height: min(80vh, 700px); display: flex; flex-direction: column; }
  header { display: flex; align-items: center; gap: 8px; padding: 12px 16px; border-bottom: 1px solid var(--line); }
  .spacer { flex: 1; }
  .body { padding: 12px 16px; display: grid; gap: 10px; overflow-y: auto; align-content: start; }
  article { border: 1px solid var(--line); border-radius: 8px; padding: 8px 10px; background: var(--bg-1); }
  article.conflict { border-left: 3px solid var(--warn, #e0af68); }
  article.error { border-left: 3px solid var(--danger); }
  .head { display: flex; gap: 8px; align-items: baseline; flex-wrap: wrap; }
  .tag { font-size: 11px; text-transform: uppercase; letter-spacing: 0.05em; color: var(--fg-2); }
  .detail { margin-top: 4px; white-space: pre-wrap; font-size: 12.5px; color: var(--fg-1); overflow-wrap: anywhere; }
  .small { font-size: 12px; }
  footer { display: flex; gap: 8px; align-items: center; padding: 10px 16px; border-top: 1px solid var(--line); }
  p { margin: 0; }
</style>
