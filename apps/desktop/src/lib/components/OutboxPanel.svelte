<script lang="ts">
  import { autofocus } from "../focus";
  import { app } from "../state.svelte";
  import { api } from "../api";
  import Icon from "./Icon.svelte";
  import type { OutboxItem } from "../types";

  let busy = $state(false);

  $effect(() => {
    if (app.outboxOpen) void app.refreshOutbox();
  });

  function when(item: OutboxItem): string {
    if (item.state === "failed") return "Stopped";
    if (item.state === "sending") return "Sending now";
    const secs = item.next_attempt_at - Math.floor(Date.now() / 1000);
    if (secs <= 5) return "Due now";
    return secs < 90 ? `Retrying in ${secs}s` : secs < 5400 ? `Retrying in ${Math.round(secs / 60)} min` : `Retrying in ${Math.round(secs / 3600)} h`;
  }

  async function act(fn: () => Promise<unknown>) {
    busy = true;
    try {
      await fn();
    } catch (e) {
      app.toast("error", typeof e === "string" ? e : String(e));
    } finally {
      await app.refreshOutbox();
      busy = false;
    }
  }
</script>

{#if app.outboxOpen}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div class="overlay" role="dialog" aria-modal="true" aria-label="Outbox" tabindex="-1" use:autofocus onkeydown={(e) => e.key === "Escape" && (app.outboxOpen = false)}>
    <div class="modal">
      <header>
        <h2>Outbox</h2>
        <span class="spacer"></span>
        <button class="btn sm" onclick={() => act(() => api.outboxFlush())} disabled={busy || app.outbox.length === 0}><Icon name="refresh" size={14} /> Try now</button>
        <button class="icon-btn" onclick={() => (app.outboxOpen = false)} aria-label="Close"><Icon name="x" /></button>
      </header>
      <div class="list">
        {#each app.outbox as item (item.id)}
          <div class="item" class:failed={item.state === "failed"}>
            <div class="main">
              <div class="subj">{item.subject || "(no subject)"}</div>
              <div class="muted small">to {item.rcpts.join(", ")}</div>
              <div class="small status">
                <span class="badge" class:danger={item.state === "failed"} class:warn={item.state !== "failed"}>{when(item)}</span>
                {#if item.attempts > 0}<span class="muted">attempt {item.attempts}</span>{/if}
              </div>
              {#if item.last_error}<div class="err small">{item.last_error}</div>{/if}
            </div>
            <div class="btns">
              {#if item.state === "failed"}<button class="btn sm" onclick={() => act(() => api.outboxRetry(item.id))} disabled={busy}>Retry</button>{/if}
              <button class="btn sm danger" onclick={() => act(() => api.outboxCancel(item.id))} disabled={busy}>Cancel</button>
            </div>
          </div>
        {:else}
          <div class="empty"><Icon name="check" size={26} /><p>Nothing waiting to be sent.</p></div>
        {/each}
      </div>
    </div>
  </div>
{/if}

<style>
  .modal { width: min(640px, 100%); }
  header { display: flex; align-items: center; gap: 8px; padding: 12px 16px; border-bottom: 1px solid var(--line); }
  .spacer { flex: 1; }
  .list { overflow-y: auto; padding: 8px 16px 16px; min-height: 120px; }
  .item { display: flex; gap: 12px; align-items: flex-start; padding: 12px 0; border-bottom: 1px solid var(--line); }
  .main { flex: 1; min-width: 0; display: grid; gap: 3px; }
  .subj { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .small { font-size: 12.5px; }
  .status { display: flex; gap: 8px; align-items: center; }
  .err { color: var(--danger); overflow-wrap: anywhere; }
  .btns { display: flex; gap: 6px; }
  .empty { display: grid; justify-items: center; gap: 4px; padding: 40px; color: var(--fg-2); }
</style>
