<script lang="ts">
  import { autofocus } from "../focus";
  import { app } from "../state.svelte";
  import { formatFullDate } from "../format";
  import Icon from "./Icon.svelte";

  $effect(() => {
    if (app.draftsOpen) void app.refreshDrafts();
  });
</script>

{#if app.draftsOpen}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div class="overlay" role="dialog" aria-modal="true" aria-label="Drafts" tabindex="-1" use:autofocus onkeydown={(e) => e.key === "Escape" && (app.draftsOpen = false)}>
    <div class="modal">
      <header>
        <h2>Drafts</h2>
        <span class="spacer"></span>
        <button class="icon-btn" onclick={() => (app.draftsOpen = false)} aria-label="Close"><Icon name="x" /></button>
      </header>
      <div class="list">
        {#each app.drafts as d (d.id)}
          <div class="item">
            <div class="main">
              <div class="subj">{d.subject || "(no subject)"}</div>
              <div class="muted small">{d.to.length ? `to ${d.to.join(", ")}` : "no recipient yet"} · {formatFullDate(d.updated_at)}</div>
            </div>
            <div class="btns">
              <button class="btn sm" onclick={() => app.openDraft(d)}>Open</button>
              <button class="btn sm danger" onclick={() => app.deleteDraft(d.id)}>Delete</button>
            </div>
          </div>
        {:else}
          <div class="empty"><Icon name="check" size={26} /><p>No saved drafts.</p></div>
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
  .item { display: flex; gap: 12px; align-items: center; padding: 12px 0; border-bottom: 1px solid var(--line); }
  .main { flex: 1; min-width: 0; display: grid; gap: 3px; }
  .subj { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .small { font-size: 12.5px; }
  .btns { display: flex; gap: 6px; }
  .empty { display: grid; justify-items: center; gap: 4px; padding: 40px; color: var(--fg-2); }
</style>
