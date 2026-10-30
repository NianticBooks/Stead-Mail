<script lang="ts">
  import { app } from "../state.svelte";
  import { cal } from "../calendar.svelte";
  import Icon from "./Icon.svelte";
  import { categoryLabel, formatListDate, isPastEvent } from "../format";
  import type { UnsubSuggestion } from "../types";

  const d = $derived(app.detail);
  const info = $derived(app.messageUnsub);
  const events = $derived(app.proposals.filter((p) => p.state === "pending" && p.method !== "CANCEL" && !isPastEvent(p)));
  const needsReply = $derived(app.aiEnabled && !!d?.triage?.needs_reply && !d?.triage?.suspicious);

  // Which sender is waiting for a second click. Unsubscribing contacts the sender, so it asks once.
  let confirming = $state<string | null>(null);
  // "Also move its messages to Trash" for the sender being confirmed. Off by default.
  let alsoTrash = $state(false);

  async function confirmUnsub(sender: string) {
    const trash = alsoTrash;
    confirming = null;
    alsoTrash = false;
    if ((await app.unsubscribe(sender)) && trash) await app.deleteSender(sender);
  }

  // Re-read the mailing-list headers when the folder, or what the server holds, changes.
  $effect(() => {
    void app.accountId;
    void app.mailbox;
    void app.currentMailbox?.last_synced_at;
    if (app.prefs.assistant && app.view === "mail") void app.refreshUnsub();
  });

  $effect(() => {
    void d?.uid;
    void d?.mailbox;
    confirming = null;
    alsoTrash = false;
    if (app.prefs.assistant) void app.loadMessageUnsub();
  });

  function how(s: { one_click: boolean; has_mailto: boolean; has_link: boolean }) {
    return s.one_click ? "one click" : s.has_mailto ? "an email" : "a web page";
  }

  function whatHappens(s: UnsubSuggestion | NonNullable<typeof info>) {
    if (s.one_click) return "Stead will send the sender a one-click request. No page opens.";
    if (s.has_mailto) return "Stead will send the sender a short unsubscribe email from this account.";
    return "Stead will open the sender's page. You confirm there.";
  }

  async function addEvents() {
    for (const p of events) if (await app.acceptProposal(p.id)) cal.markStale();
  }

  function draftReply() {
    app.reply();
    if (app.compose) app.compose.ai.open = true;
  }
</script>

<aside class="assistant" aria-label="Assistant">
  <header>
    <span class="title"><Icon name="sparkles" size={15} /> Assistant</span>
    <button class="icon-btn" onclick={() => app.setPref("assistant", false)} title="Hide (a)" aria-label="Hide the assistant"><Icon name="x" size={14} /></button>
  </header>

  <div class="scroll">
    <section aria-label="This message">
      <h3>This message</h3>
      {#if !d}
        <p class="muted small">Open a message to see what can be done with it.</p>
      {:else}
        {#if events.length}
          <button class="action" onclick={addEvents}>
            <Icon name="calendar" size={15} />
            <span><strong>Add {events.length === 1 ? "the event" : `${events.length} events`} to your calendar</strong><small>{events[0].title}{events.length > 1 ? ` and ${events.length - 1} more` : ""}</small></span>
          </button>
        {/if}
        {#if needsReply}
          <button class="action" onclick={draftReply}>
            <Icon name="reply" size={15} />
            <span><strong>Draft a reply</strong><small>Written by your local model, for you to edit</small></span>
          </button>
        {/if}
        {#if info && info.state === null}
          {#if confirming === info.sender}
            <div class="confirm" role="alertdialog" aria-label="Confirm unsubscribe">
              <p><strong>Unsubscribe from {info.name ?? info.sender}?</strong> {whatHappens(info)}</p>
              {#if !app.inTrash && info.total > 0}
                <label class="also"><input type="checkbox" bind:checked={alsoTrash} /> Also move its {info.total} message{info.total === 1 ? "" : "s"} in this folder to Trash</label>
              {/if}
              <div class="row">
                <button class="btn sm primary" disabled={app.unsubBusy !== null} onclick={() => void confirmUnsub(info.sender)}>
                  {app.unsubBusy === info.sender ? "Working…" : "Unsubscribe"}
                </button>
                <button class="btn sm ghost" onclick={() => (confirming = null)}>Cancel</button>
              </div>
            </div>
          {:else}
            <button class="action" onclick={() => { alsoTrash = false; confirming = info.sender; }}>
              <Icon name="mail" size={15} />
              <span><strong>Unsubscribe from {info.name ?? info.sender}</strong><small>{info.total} message{info.total === 1 ? "" : "s"} here · via {how(info)}</small></span>
            </button>
          {/if}
        {:else if info?.state === "unsubscribed" || info?.state === "opened"}
          <p class="done"><Icon name="check" size={14} /> {info.state === "opened" ? "You opened this sender's unsubscribe page." : "You unsubscribed from this sender."}</p>
          {#if !app.inTrash}
            <button class="action" onclick={() => void app.deleteSender(info.sender)}>
              <Icon name="trash" size={15} />
              <span><strong>Move its messages to Trash</strong><small>Every message from {info.name ?? info.sender} in this folder. You can undo.</small></span>
            </button>
          {/if}
        {/if}
        {#if !events.length && !needsReply && !info}
          <p class="muted small">Nothing to suggest for this message.</p>
        {/if}
      {/if}
    </section>

    <section aria-label="Unsubscribe suggestions">
      <h3>Unsubscribe suggestions {#if app.unsubLoading}<Icon name="loader" size={12} />{/if}</h3>
      {#if app.unsub.length === 0}
        <p class="muted small">{app.unsubLoading ? "Looking through this folder…" : "No mailing lists with several messages in this folder."}</p>
      {:else}
        <p class="muted small">Senders in this folder with an unsubscribe option, most mail first.</p>
        <ul>
          {#each app.unsub as s (s.sender)}
            <li class="sender">
              <div class="who">
                <strong title={s.sender}>{s.name ?? s.sender}</strong>
                <span class="muted small">{s.total} messages · {s.unread} unread{s.last_date ? ` · last ${formatListDate(s.last_date)}` : ""}</span>
                {#if s.category}<span class="chip">{categoryLabel[s.category] ?? s.category}</span>{/if}
              </div>
              {#if confirming === s.sender}
                <p class="small">{whatHappens(s)}</p>
                {#if !app.inTrash}
                  <label class="also"><input type="checkbox" bind:checked={alsoTrash} /> Also move its {s.total} messages in this folder to Trash</label>
                {/if}
                <div class="row">
                  <button class="btn sm primary" disabled={app.unsubBusy !== null} onclick={() => void confirmUnsub(s.sender)}>
                    {app.unsubBusy === s.sender ? "Working…" : "Unsubscribe"}
                  </button>
                  <button class="btn sm ghost" onclick={() => (confirming = null)}>Cancel</button>
                </div>
              {:else}
                <div class="row">
                  <button class="btn sm" onclick={() => { alsoTrash = false; confirming = s.sender; }} title="Unsubscribe via {how(s)}">Unsubscribe</button>
                  <button class="btn sm ghost" onclick={() => app.dismissUnsub(s.sender)} title="Stop suggesting this sender">Not now</button>
                </div>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  </div>
</aside>

<style>
  .assistant { display: flex; flex-direction: column; min-height: 0; border-left: 1px solid var(--line); background: var(--bg-0); }
  header { display: flex; align-items: center; justify-content: space-between; padding: 12px 12px 8px 14px; }
  .title { display: inline-flex; align-items: center; gap: 8px; font-weight: 600; }
  .title :global(svg) { color: var(--accent); }
  .scroll { overflow-y: auto; padding: 0 12px 16px; display: grid; gap: 20px; align-content: start; }
  section { display: grid; gap: 8px; }
  h3 { font-size: 12px; font-weight: 600; letter-spacing: 0.04em; text-transform: uppercase; color: var(--fg-2); display: flex; gap: 6px; align-items: center; }
  p { margin: 0; line-height: 1.45; }
  ul { list-style: none; margin: 0; padding: 0; display: grid; gap: 8px; }
  .action {
    display: flex; gap: 10px; align-items: flex-start; text-align: left; padding: 9px 10px; width: 100%;
    background: var(--bg-1); border: 1px solid var(--line); border-radius: var(--radius); color: var(--fg-0); cursor: pointer;
  }
  .action:hover { border-color: var(--accent); background: var(--accent-bg); }
  .action :global(svg) { margin-top: 2px; color: var(--accent); flex: none; }
  .action span { display: grid; gap: 2px; min-width: 0; }
  .action small { color: var(--fg-2); font-size: 12px; overflow-wrap: anywhere; }
  .confirm, .sender { display: grid; gap: 8px; padding: 9px 10px; background: var(--bg-1); border: 1px solid var(--line); border-radius: var(--radius); }
  .confirm { border-color: var(--warn); }
  .who { display: grid; gap: 3px; min-width: 0; }
  .who strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .row { display: flex; gap: 8px; flex-wrap: wrap; }
  .chip { justify-self: start; padding: 1px 9px; border-radius: 999px; font-size: 12px; font-weight: 600; background: var(--bg-3); color: var(--fg-1); }
  .also { display: flex; gap: 8px; align-items: flex-start; font-size: 13px; line-height: 1.4; cursor: pointer; }
  .also input { margin-top: 2px; }
  .done { display: flex; gap: 6px; align-items: center; color: var(--ok); font-size: 13px; }
</style>
