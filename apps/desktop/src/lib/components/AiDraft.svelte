<script lang="ts">
  import { app } from "../state.svelte";
  import Icon from "./Icon.svelte";
  import { formatBytes } from "../format";

  const c = $derived(app.compose);
  const cloudReady = $derived(!!app.aiStatus?.config.cloud_enabled && !!app.aiStatus?.cloud_key_present);

  $effect(() => {
    if (c?.ai.open && !app.aiStatus) void app.refreshAiStatus();
  });
</script>

{#if app.aiEnabled && c?.replyTo}
  <div class="ai-draft">
    {#if !c.ai.open}
      <button class="btn sm ghost" onclick={() => (c.ai.open = true)}><Icon name="sparkles" size={14} /> Draft with AI</button>
    {:else}
      <div class="panel">
        <div class="head">
          <Icon name="sparkles" size={14} /> <strong>Draft with AI</strong>
          <span class="spacer"></span>
          <button class="icon-btn tiny" onclick={() => (c.ai.open = false)} aria-label="Close AI drafting"><Icon name="x" size={13} /></button>
        </div>
        <label class="field">
          <span class="muted small">What do you want to say? <span class="muted">(optional)</span></span>
          <input
            class="input"
            bind:value={c.ai.instruction}
            placeholder="e.g. say yes, but suggest 7 instead"
            onkeydown={(e) => e.key === "Enter" && !e.ctrlKey && !e.metaKey && (e.preventDefault(), app.draftWithAi())}
          />
        </label>
        <div class="row">
          <select class="input provider" bind:value={c.ai.provider} aria-label="Where to write the draft">
            <option value="local">On this computer</option>
            <option value="cloud" disabled={!cloudReady}>Claude (cloud){cloudReady ? "" : " — off"}</option>
          </select>
          <button class="btn sm primary" onclick={() => app.draftWithAi()} disabled={c.ai.busy}>
            <Icon name={c.ai.busy ? "loader" : "sparkles"} size={14} />
            {c.ai.busy ? "Writing…" : c.ai.provider === "cloud" ? "Review what is sent…" : "Write draft"}
          </button>
          {#if c.ai.undo !== null}<button class="btn sm ghost" onclick={() => app.undoDraft()}>Undo draft</button>{/if}
        </div>
        {#if c.ai.provider === "local"}<p class="note muted">The message stays on this computer.</p>{/if}
        {#if c.ai.error}<div class="error" role="alert"><Icon name="alert" size={14} /> {c.ai.error}</div>{/if}

        {#if c.ai.preview}
          <div class="consent" role="alertdialog" aria-label="Confirm what will be sent to the cloud">
            <p><strong>This will send {formatBytes(c.ai.preview.bytes)} to {c.ai.preview.model} at Anthropic.</strong> Only what is shown below leaves your computer: your instruction and the start of the message you are replying to.</p>
            <details>
              <summary>Show exactly what is sent</summary>
              <p class="label">Instructions to the model</p>
              <pre>{c.ai.preview.system}</pre>
              <p class="label">Message</p>
              <pre>{c.ai.preview.user}</pre>
            </details>
            <div class="row">
              <button class="btn sm primary" onclick={() => app.confirmCloudDraft()} disabled={c.ai.busy}>Send to Claude</button>
              <button class="btn sm ghost" onclick={() => (c.ai.preview = null)}>Cancel</button>
            </div>
          </div>
        {/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  .ai-draft { padding: 0 16px 8px; }
  .panel { display: grid; gap: 8px; padding: 10px 12px; background: var(--bg-1); border: 1px solid var(--line-strong); border-radius: var(--radius); }
  .head { display: flex; align-items: center; gap: 8px; }
  .head :global(svg) { color: var(--accent); }
  .spacer { flex: 1; }
  .tiny { width: 22px; height: 22px; }
  .row { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .provider { width: auto; min-width: 190px; }
  .small { font-size: 12.5px; }
  .note { margin: 0; font-size: 12px; }
  .error { display: flex; gap: 8px; align-items: flex-start; color: var(--danger); font-size: 12.5px; overflow-wrap: anywhere; }
  .consent { display: grid; gap: 8px; padding: 10px 12px; border-radius: var(--radius-sm); background: color-mix(in srgb, var(--warn) 9%, transparent); border: 1px solid color-mix(in srgb, var(--warn) 40%, transparent); }
  .consent p { margin: 0; line-height: 1.5; }
  .label { margin-top: 8px !important; color: var(--fg-2); font-size: 12px; }
  summary { cursor: pointer; color: var(--accent); }
  pre { margin: 4px 0 0; max-height: 180px; overflow: auto; padding: 8px; background: var(--bg-0); border: 1px solid var(--line); border-radius: var(--radius-sm); white-space: pre-wrap; overflow-wrap: anywhere; }
</style>
