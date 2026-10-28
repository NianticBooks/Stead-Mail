<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  type Account = { id: string; name: string; email: string; provider: string; imap: { host: string; port: number } };
  type Mailbox = { id: number; name: string; exists_count: number | null; last_synced_at: string | null };
  type Message = {
    id: number; uid: number; subject: string | null; from_name: string | null; from_addr: string | null;
    date: string | null; internal_date: string | null; flags: string;
  };
  type Diag = {
    os: string; session_type: string | null; wayland_display: string | null; hyprland: boolean;
    webkit_disable_dmabuf: string | null; nv_disable_explicit_sync: string | null; webkit_disable_compositing: string | null;
    core: { version: string; data_dir: string; secrets_backend: string; accounts: number } | null; core_error: string | null;
  };

  let diag = $state<Diag | null>(null);
  let accounts = $state<Account[]>([]);
  let mailboxes = $state<Mailbox[]>([]);
  let messages = $state<Message[]>([]);
  let selectedAccount = $state<Account | null>(null);
  let selectedMailbox = $state<string>("INBOX");
  let busy = $state(false);
  let error = $state<string | null>(null);
  let renderedAt = $state("");

  onMount(async () => {
    renderedAt = new Date().toLocaleTimeString();
    diag = await invoke<Diag>("diag");
    accounts = await invoke<Account[]>("list_accounts");
    if (accounts.length) await pickAccount(accounts[0]);
  });

  async function pickAccount(a: Account) {
    selectedAccount = a;
    error = null;
    try {
      mailboxes = await invoke<Mailbox[]>("list_mailboxes", { account: a.id });
      await pickMailbox(mailboxes.find((m) => m.name.toUpperCase() === "INBOX")?.name ?? mailboxes[0]?.name ?? "INBOX");
    } catch (e) { error = String(e); }
  }

  async function pickMailbox(name: string) {
    selectedMailbox = name;
    if (!selectedAccount) return;
    try {
      messages = await invoke<Message[]>("list_messages", { account: selectedAccount.id, mailbox: name, limit: 100 });
    } catch (e) { messages = []; error = String(e); }
  }

  async function sync() {
    if (!selectedAccount) return;
    busy = true; error = null;
    try {
      await invoke("sync_mailbox", { account: selectedAccount.id, mailbox: selectedMailbox, limit: 50 });
      await pickAccount(selectedAccount);
    } catch (e) { error = String(e); } finally { busy = false; }
  }

  const fmtDate = (m: Message) => {
    const d = m.date ?? m.internal_date;
    return d ? new Date(d).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }) : "";
  };
</script>

<div class="shell">
  <aside class="rail">
    <div class="brand">Mailcore <span class="tag">phase 0</span></div>
    <h3>Accounts</h3>
    {#if accounts.length === 0}
      <p class="muted">No accounts yet.<br />Add one from the terminal:</p>
      <pre>mailcore account add \
  --name icloud \
  --email you@icloud.com \
  --provider icloud</pre>
    {/if}
    {#each accounts as a}
      <button class="item" class:active={selectedAccount?.id === a.id} onclick={() => pickAccount(a)}>
        <strong>{a.name}</strong><br /><span class="muted">{a.email}</span>
      </button>
    {/each}
    {#if selectedAccount}
      <h3>Mailboxes</h3>
      {#each mailboxes as m}
        <button class="item" class:active={selectedMailbox === m.name} onclick={() => pickMailbox(m.name)}>
          {m.name} <span class="muted right">{m.exists_count ?? ""}</span>
        </button>
      {/each}
    {/if}
  </aside>

  <main class="list">
    <header>
      <span>{selectedAccount ? `${selectedAccount.name} / ${selectedMailbox}` : "Inbox"}</span>
      <button onclick={sync} disabled={busy || !selectedAccount}>{busy ? "Syncing…" : "Sync"}</button>
    </header>
    {#if error}<div class="error">{error}</div>{/if}
    {#each messages as m}
      <div class="msg" class:unread={!m.flags.includes("\\Seen")}>
        <div class="from">{m.from_name ?? m.from_addr ?? "(unknown)"}</div>
        <div class="subject">{m.subject ?? "(no subject)"}</div>
        <div class="date muted">{fmtDate(m)}</div>
      </div>
    {:else}
      <p class="muted pad">Nothing cached for this mailbox. Press Sync.</p>
    {/each}
  </main>

  <section class="detail">
    <h3>Spike diagnostics</h3>
    <p class="muted">If you can read this, WebKitGTK rendered on this GPU and the core answered over IPC.</p>
    <div class="ok">Rendered at {renderedAt}</div>
    {#if diag}
      <table>
        <tbody>
          <tr><td>OS</td><td>{diag.os}</td></tr>
          <tr><td>Session</td><td>{diag.session_type ?? "?"} {diag.wayland_display ? `(${diag.wayland_display})` : ""}</td></tr>
          <tr><td>Hyprland</td><td>{diag.hyprland ? "yes" : "no"}</td></tr>
          <tr><td>WEBKIT_DISABLE_DMABUF_RENDERER</td><td>{diag.webkit_disable_dmabuf ?? "unset"}</td></tr>
          <tr><td>__NV_DISABLE_EXPLICIT_SYNC</td><td>{diag.nv_disable_explicit_sync ?? "unset"}</td></tr>
          <tr><td>WEBKIT_DISABLE_COMPOSITING_MODE</td><td>{diag.webkit_disable_compositing ?? "unset"}</td></tr>
          {#if diag.core}
            <tr><td>Core</td><td>mailcore {diag.core.version}</td></tr>
            <tr><td>Data dir</td><td><code>{diag.core.data_dir}</code></td></tr>
            <tr><td>Secrets</td><td>{diag.core.secrets_backend}</td></tr>
            <tr><td>Accounts</td><td>{diag.core.accounts}</td></tr>
          {:else}
            <tr><td>Core</td><td class="bad">{diag.core_error}</td></tr>
          {/if}
        </tbody>
      </table>
    {:else}
      <p class="muted">Waiting for core…</p>
    {/if}
  </section>
</div>

<style>
  .shell { display: grid; grid-template-columns: 240px minmax(320px, 1fr) minmax(320px, 1.2fr); height: 100vh; }
  .rail { background: var(--bg-1); border-right: 1px solid var(--line); padding: 14px 10px; overflow-y: auto; }
  .brand { font-weight: 700; font-size: 16px; padding: 4px 8px 14px; letter-spacing: 0.2px; }
  .tag { font-size: 10px; font-weight: 600; color: var(--accent); border: 1px solid var(--accent); border-radius: 999px; padding: 1px 7px; vertical-align: middle; margin-left: 6px; }
  h3 { font-size: 11px; text-transform: uppercase; letter-spacing: 0.08em; color: var(--fg-2); margin: 14px 8px 6px; }
  .item { display: block; width: 100%; text-align: left; background: transparent; border: 1px solid transparent; padding: 7px 8px; border-radius: var(--radius); }
  .item:hover { background: var(--bg-2); }
  .item.active { background: var(--bg-3); border-color: var(--line); }
  .right { float: right; }
  .muted { color: var(--fg-2); }
  .pad { padding: 16px; }
  pre { background: var(--bg-0); border: 1px solid var(--line); border-radius: var(--radius); padding: 8px; margin: 6px 0 0; white-space: pre; overflow-x: auto; }
  .list { border-right: 1px solid var(--line); overflow-y: auto; }
  header { position: sticky; top: 0; display: flex; justify-content: space-between; align-items: center; padding: 10px 14px; background: var(--bg-0); border-bottom: 1px solid var(--line); font-weight: 600; }
  .msg { display: grid; grid-template-columns: 1fr auto; grid-template-areas: "from date" "subject subject"; padding: 10px 14px; border-bottom: 1px solid var(--line); }
  .msg:hover { background: var(--bg-1); }
  .msg.unread .from, .msg.unread .subject { font-weight: 600; color: var(--fg-0); }
  .from { grid-area: from; color: var(--fg-1); }
  .subject { grid-area: subject; color: var(--fg-1); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .date { grid-area: date; font-size: 12px; }
  .detail { padding: 16px 20px; overflow-y: auto; }
  .ok { display: inline-block; color: var(--accent-2); border: 1px solid var(--accent-2); border-radius: 999px; padding: 2px 10px; font-size: 12px; margin: 6px 0 12px; }
  .bad { color: var(--danger); }
  .error { margin: 10px 14px; padding: 8px 10px; border: 1px solid var(--danger); border-radius: var(--radius); color: var(--danger); background: color-mix(in srgb, var(--danger) 10%, transparent); }
  table { border-collapse: collapse; width: 100%; }
  td { padding: 5px 8px; border-bottom: 1px solid var(--line); vertical-align: top; }
  td:first-child { color: var(--fg-2); white-space: nowrap; width: 40%; }
</style>
