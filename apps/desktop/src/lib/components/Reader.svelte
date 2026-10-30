<script lang="ts">
  import { app } from "../state.svelte";
  import Icon from "./Icon.svelte";
  import Avatar from "./Avatar.svelte";
  import HtmlFrame from "./HtmlFrame.svelte";
  import AiCard from "./AiCard.svelte";
  import { linkify } from "../linkify";
  import { formatFullDate, formatListDate, formatSize, isStarred, isUnread, senderLabel } from "../format";

  const d = $derived(app.detail);
  const attachments = $derived(d?.attachments.filter((a) => !a.inline || a.filename) ?? []);
</script>

<section class="reader" aria-label="Message">
  {#if app.loadingMessage && !d}
    <div class="pad">
      <div class="skeleton" style:width="60%" style:height="22px"></div>
      <div class="skeleton" style:width="35%" style:height="14px" style:margin-top="16px"></div>
      <div class="skeleton" style:width="90%" style:height="12px" style:margin-top="28px"></div>
      <div class="skeleton" style:width="80%" style:height="12px" style:margin-top="10px"></div>
      <div class="skeleton" style:width="85%" style:height="12px" style:margin-top="10px"></div>
    </div>
  {:else if app.messageError}
    <div class="state">
      <Icon name="alert" size={28} />
      <p>Couldn’t load this message.</p>
      <p class="muted">{app.messageError}</p>
    </div>
  {:else if !d}
    <div class="state">
      <Icon name="mail" size={34} />
      <p>Select a message to read it.</p>
      <p class="muted"><kbd>j</kbd> <kbd>k</kbd> to move &nbsp; <kbd>c</kbd> to compose &nbsp; <kbd>/</kbd> to search</p>
    </div>
  {:else}
    <header>
      <div class="subject-row">
        <h1>{d.subject || "(no subject)"}</h1>
        <div class="actions">
          <button class="btn sm" onclick={() => app.reply()} title="Reply (r)"><Icon name="reply" size={14} /> Reply</button>
          <button class="btn sm" onclick={() => void app.replyAll()} title="Reply to everyone (Shift+R)">Reply all</button>
          <button class="icon-btn" onclick={() => app.setSeen(d.uid, isUnread(d))} title={isUnread(d) ? "Mark as read (u)" : "Mark as unread (u)"} aria-label={isUnread(d) ? "Mark as read" : "Mark as unread"}>
            <Icon name="mail" fill={isUnread(d)} />
          </button>
          <button class="icon-btn" class:on={isStarred(d)} onclick={() => app.setStarred(d.uid, !isStarred(d))} title="Star (s)" aria-label="Star">
            <Icon name="star" fill={isStarred(d)} />
          </button>
          <button class="icon-btn" onclick={() => void app.deleteMessage()} title="Move to Trash (#)" aria-label="Move to Trash">
            <Icon name="trash" />
          </button>
        </div>
      </div>

      <div class="sender">
        <Avatar label={senderLabel(d)} size={36} />
        <div class="meta">
          <div><strong>{senderLabel(d)}</strong> {#if d.from_name && d.from_addr}<span class="muted addr">&lt;{d.from_addr}&gt;</span>{/if}</div>
          {#if d.to_addrs}<div class="muted small">to {d.to_addrs}</div>{/if}
          <div class="muted small">{formatFullDate(d.date ?? d.internal_date)}</div>
        </div>
      </div>

      {#if app.thread.length > 1}
        <div class="thread" aria-label="Conversation">
          <span class="muted small">Conversation · {app.thread.length}</span>
          {#each app.thread as t (t.id)}
            <button class="chip" class:current={t.uid === d.uid && t.mailbox_id === d.mailbox_id} onclick={() => app.openMessage(t)} title={t.subject ?? ""}>
              {senderLabel(t)} <span class="muted">{formatListDate(t.date ?? t.internal_date)}</span>
            </button>
          {/each}
        </div>
      {/if}
    </header>

    <div class="body">
      <AiCard />
      {#if attachments.length}
        <div class="attachments" aria-label="Attachments">
          {#each attachments as a (a.part_index)}
            <button class="att" onclick={() => app.saveAttachment(a.part_index)} title="Save to Downloads">
              <Icon name="clip" size={14} />
              <span class="name">{a.filename ?? "(unnamed)"}</span>
              <span class="muted size">{formatSize(a.size)}</span>
              <Icon name="download" size={14} />
            </button>
          {/each}
        </div>
      {/if}

      {#if app.html && app.html.blocked_images > 0 && !app.allowImages}
        <div class="banner">
          <Icon name="shield" size={15} />
          <span>{app.html.blocked_images} remote image{app.html.blocked_images === 1 ? "" : "s"} blocked. Loading them tells the sender you opened this message.</span>
          <button class="btn sm" onclick={() => app.loadRemoteImages()}>Load images</button>
        </div>
      {/if}

      {#if app.html}
        <HtmlFrame html={app.html.html} allowImages={app.allowImages} onlink={(u) => app.openLink(u)} />
      {:else if d.body_text}
        <div class="plain">{#each linkify(d.body_text) as p}{#if p.href}<a
              href={p.href}
              onclick={(e) => {
                e.preventDefault();
                void app.openLink(p.href!);
              }}>{p.text}</a>{:else}{p.text}{/if}{/each}</div>
      {:else}
        <p class="muted pad-sm">This message has no readable body.</p>
      {/if}

    </div>
  {/if}
</section>

<style>
  .reader { display: flex; flex-direction: column; min-height: 0; min-width: 0; background: var(--bg-0); overflow-y: auto; overflow-x: hidden; }
  .pad { padding: 28px; }
  .pad-sm { padding: 16px 0; }
  .state { display: grid; justify-items: center; align-content: center; gap: 4px; height: 100%; color: var(--fg-2); text-align: center; padding: 24px; }
  .state p { margin: 6px 0 0; }
  header { min-width: 0; padding: 16px 22px 12px; border-bottom: 1px solid var(--line); display: grid; grid-template-columns: minmax(0, 1fr); gap: 14px; position: sticky; top: 0; background: var(--bg-0); z-index: 2; }
  .subject-row { display: flex; flex-wrap: wrap; align-items: flex-start; justify-content: space-between; gap: 8px 16px; }
  h1 { flex: 1 1 200px; min-width: 0; font-size: 18px; line-height: 1.3; overflow-wrap: anywhere; }
  .actions { display: flex; align-items: center; gap: 4px; flex: none; }
  .sender { display: flex; align-items: center; gap: 12px; }
  .meta { flex: 1; min-width: 0; line-height: 1.35; }
  .meta div { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .addr { font-size: 12.5px; }
  .small { font-size: 12.5px; }
  .thread { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; }
  .chip { padding: 3px 9px; background: var(--bg-2); border: 1px solid var(--line-strong); border-radius: 999px; color: var(--fg-1); cursor: pointer; font-size: 12.5px; max-width: 260px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .chip:hover { border-color: var(--accent); }
  .chip.current { background: var(--accent-bg); border-color: var(--accent); color: var(--fg-0); }
  .body { min-width: 0; padding: 14px 22px 40px; display: grid; grid-template-columns: minmax(0, 1fr); gap: 14px; align-content: start; }
  .banner {
    display: flex; flex-wrap: wrap; align-items: center; gap: 8px 10px; padding: 8px 12px; border-radius: var(--radius); font-size: 12.5px;
    background: color-mix(in srgb, var(--warn) 10%, transparent); border: 1px solid color-mix(in srgb, var(--warn) 30%, transparent); color: var(--fg-1);
  }
  .banner span { flex: 1 1 180px; }
  .plain { white-space: pre-wrap; overflow-wrap: anywhere; font-size: 14px; line-height: 1.6; color: var(--fg-0); max-width: 78ch; }
  .attachments { display: flex; flex-wrap: wrap; gap: 8px; }
  .att {
    display: inline-flex; align-items: center; gap: 8px; padding: 7px 12px; background: var(--bg-2); color: var(--fg-1);
    border: 1px solid var(--line-strong); border-radius: var(--radius); cursor: pointer; max-width: 100%; min-width: 0;
  }
  .att .size { white-space: nowrap; font-size: 12px; }
  .att:hover { border-color: var(--accent); color: var(--fg-0); }
  .att .name { flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
