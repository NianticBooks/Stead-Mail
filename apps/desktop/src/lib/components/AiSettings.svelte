<script lang="ts">
  import { app } from "../state.svelte";
  import { api } from "../api";
  import Icon from "./Icon.svelte";
  import { formatBytes } from "../format";
  import type { AiConfig, AuditReport } from "../types";

  let cfg = $state<AiConfig | null>(null);
  let key = $state("");
  let busy = $state(false);
  let message = $state<string | null>(null);
  let error = $state<string | null>(null);
  let audit = $state<AuditReport | null>(null);

  async function load() {
    await app.refreshAiStatus();
    if (app.aiConfig) cfg = { ...app.aiConfig };
    try {
      audit = await api.aiAudit(15);
    } catch {
      /* the log is informational */
    }
  }

  $effect(() => {
    if (cfg === null) void load();
  });

  const s = $derived(app.aiStatus);
  const local = $derived(s?.local);

  async function save(next: AiConfig, note = "Saved") {
    busy = true;
    error = null;
    message = null;
    try {
      await app.saveAiConfig(next);
      cfg = { ...app.aiConfig! };
      message = note;
    } catch (e) {
      error = typeof e === "string" ? e : String(e);
    } finally {
      busy = false;
    }
  }

  async function storeKey() {
    busy = true;
    error = null;
    try {
      await api.aiSetKey(key);
      key = "";
      await load();
      message = "Key stored in the system keyring";
    } catch (e) {
      error = typeof e === "string" ? e : String(e);
    } finally {
      busy = false;
    }
  }

  async function removeKey() {
    await api.aiClearKey();
    if (cfg?.cloud_enabled) await save({ ...cfg, cloud_enabled: false, extract_provider: "local", draft_provider: "local" }, "Key removed; cloud models are off");
    else await load();
  }

  const cloudUsage = $derived(s?.usage.find((u) => u.provider === "cloud"));
  const localUsage = $derived(s?.usage.find((u) => u.provider === "local"));
</script>

<h3>AI</h3>
<p class="muted lead">Sort mail, spot events and draft replies. Everything is a suggestion: nothing is sent, moved or added to your calendar without you clicking.</p>

{#if cfg && s}
  {#if error}<div class="error" role="alert"><Icon name="alert" size={15} /> {error}</div>{/if}

  <section class="card">
    <label class="toggle">
      <input type="checkbox" checked={cfg.enabled} disabled={busy} onchange={(e) => save({ ...cfg!, enabled: e.currentTarget.checked }, e.currentTarget.checked ? "AI is on" : "AI is off")} />
      <span><strong>Use AI on my mail</strong><br /><span class="muted">Runs on this computer with Ollama. Messages never leave it unless you also turn on cloud models below.</span></span>
    </label>
  </section>

  <section class="card">
    <h4>This computer (Ollama)</h4>
    <p class="status" class:ok={local?.reachable && local?.model_installed} class:bad={!local?.reachable}>
      <Icon name={local?.reachable && local?.model_installed ? "check" : "alert"} size={15} />
      {#if !local?.reachable}Not reachable. Is Ollama running? ({local?.error ?? "no answer"})
      {:else if !local.model_installed}Reachable, but “{cfg.local_model}” is not installed. Run <code>ollama pull {cfg.local_model}</code>
      {:else}Ready: {cfg.local_model}{/if}
    </p>
    <div class="grid">
      <label class="field"><span>Model</span>
        {#if local && local.installed_models.length}
          <select class="input" bind:value={cfg.local_model}>
            {#each local.installed_models as m (m)}<option value={m}>{m}</option>{/each}
            {#if !local.installed_models.includes(cfg.local_model)}<option value={cfg.local_model}>{cfg.local_model} (not installed)</option>{/if}
          </select>
        {:else}
          <input class="input" bind:value={cfg.local_model} />
        {/if}
      </label>
      <label class="field"><span>Address</span><input class="input" bind:value={cfg.ollama_url} spellcheck="false" /></label>
      <label class="field"><span>Time zone <span class="muted">(blank = this computer’s)</span></span><input class="input" bind:value={cfg.timezone} placeholder="America/Chicago" spellcheck="false" /></label>
    </div>
    <div class="btns"><button class="btn sm" onclick={() => save(cfg!)} disabled={busy}>Save</button><button class="btn sm ghost" onclick={load}>Check again</button></div>
    {#if message}<span class="ok-note">{message}</span>{/if}
  </section>

  <section class="card">
    <h4>Cloud models <span class="badge warn">off by default</span></h4>
    <p class="muted">Optional. Reading dates uses Claude Haiku and drafting uses Claude Sonnet, via your own Anthropic API key. Event reading sends only the sentences that mention dates. Drafts always show you exactly what will be sent before anything leaves this computer.</p>
    <div class="keyrow">
      {#if s.cloud_key_present}
        <span class="status ok"><Icon name="check" size={15} /> API key stored in the system keyring</span>
        <button class="btn sm" onclick={removeKey} disabled={busy}>Remove key</button>
      {:else}
        <input class="input" type="password" placeholder="sk-ant-…" bind:value={key} autocomplete="off" aria-label="Anthropic API key" />
        <button class="btn sm" onclick={storeKey} disabled={busy || key.length < 20}>Store key</button>
      {/if}
    </div>
    <label class="toggle">
      <input type="checkbox" checked={cfg.cloud_enabled} disabled={busy || !s.cloud_key_present} onchange={(e) => save({ ...cfg!, cloud_enabled: e.currentTarget.checked, ...(e.currentTarget.checked ? {} : { extract_provider: "local", draft_provider: "local" }) } as AiConfig, e.currentTarget.checked ? "Cloud models allowed" : "Cloud models are off")} />
      <span>Allow cloud models{s.cloud_key_present ? "" : " (add a key first)"}</span>
    </label>
    {#if cfg.cloud_enabled}
      <div class="grid">
        <label class="field"><span>Read events with</span><select class="input" bind:value={cfg.extract_provider} onchange={() => save(cfg!)}><option value="local">This computer</option><option value="cloud">Claude Haiku (dates only)</option></select></label>
        <label class="field"><span>Write drafts with</span><select class="input" bind:value={cfg.draft_provider} onchange={() => save(cfg!)}><option value="local">This computer</option><option value="cloud">Claude Sonnet (you confirm each time)</option></select></label>
      </div>
    {/if}
  </section>

  <section class="card">
    <h4>What has been sent where <span class="muted small">(last 30 days)</span></h4>
    <div class="usage">
      <div><strong>{localUsage?.calls ?? 0}</strong><span class="muted">calls on this computer</span></div>
      <div class:cloud={(cloudUsage?.calls ?? 0) > 0}><strong>{cloudUsage ? formatBytes(cloudUsage.bytes_out) : "0 B"}</strong><span class="muted">sent to the cloud in {cloudUsage?.calls ?? 0} calls</span></div>
    </div>
    {#if audit && audit.calls.length}
      <details>
        <summary>Recent calls</summary>
        <table>
          <thead><tr><th>When</th><th>Where</th><th>Task</th><th>Sent</th><th>Time</th></tr></thead>
          <tbody>
            {#each audit.calls as c (c.id)}
              <tr class:failed={!c.ok}><td>{new Date(c.at).toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" })}</td><td>{c.provider === "cloud" ? "Cloud" : "Local"} · {c.model}</td><td>{c.task}</td><td>{formatBytes(c.bytes_out)}</td><td>{c.ok ? `${c.millis} ms` : "failed"}</td></tr>
            {/each}
          </tbody>
        </table>
        <p class="muted small">The log records sizes only, never message content.</p>
      </details>
    {/if}
  </section>
{:else}
  <p class="muted">Loading…</p>
{/if}

<style>
  .lead { margin: -6px 0 6px; line-height: 1.5; }
  .card { display: grid; gap: 10px; padding: 14px 16px; background: var(--bg-1); border: 1px solid var(--line); border-radius: var(--radius); }
  h4 { margin: 0; font-size: 13.5px; display: flex; align-items: center; gap: 8px; }
  .card > p { margin: 0; line-height: 1.5; }
  .toggle { display: flex; gap: 12px; align-items: flex-start; cursor: pointer; line-height: 1.45; }
  .toggle input { margin-top: 3px; }
  .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 10px; }
  .btns { display: flex; gap: 8px; align-items: center; }
  .status { display: flex; align-items: flex-start; gap: 8px; margin: 0; color: var(--fg-1); overflow-wrap: anywhere; }
  .status.ok { color: var(--ok); }
  .status.bad { color: var(--danger); }
  .ok-note { color: var(--ok); font-size: 12.5px; }
  .keyrow { display: flex; gap: 8px; align-items: center; }
  .keyrow .input { flex: 1; }
  .usage { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
  .usage div { display: grid; gap: 2px; padding: 10px 12px; background: var(--bg-0); border: 1px solid var(--line); border-radius: var(--radius-sm); }
  .usage strong { font-size: 18px; }
  .usage .cloud { border-color: color-mix(in srgb, var(--warn) 50%, transparent); }
  summary { cursor: pointer; color: var(--accent); }
  table { width: 100%; border-collapse: collapse; margin-top: 8px; font-size: 12.5px; }
  th { text-align: left; color: var(--fg-2); font-weight: 500; padding: 4px 6px; }
  td { padding: 4px 6px; border-top: 1px solid var(--line); }
  tr.failed td { color: var(--danger); }
  code { background: var(--bg-3); padding: 1px 5px; border-radius: 4px; }
  .error { display: flex; gap: 8px; padding: 8px 10px; border-radius: var(--radius-sm); color: var(--danger); background: color-mix(in srgb, var(--danger) 10%, transparent); border: 1px solid color-mix(in srgb, var(--danger) 35%, transparent); overflow-wrap: anywhere; }
  .small { font-size: 12px; }
</style>
