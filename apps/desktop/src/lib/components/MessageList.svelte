<script lang="ts">
  import { app } from "../state.svelte";
  import Icon from "./Icon.svelte";
  import { categoryLabel, formatListDate, isStarred, isUnread, recipientLabel, senderLabel } from "../format";
  import type { MessageRow, SearchHit } from "../types";

  let searchBox = $state<HTMLInputElement | null>(null);
  let rowsEl = $state<HTMLElement | null>(null);

  // Keep the selected row visible while stepping with j/k.
  $effect(() => {
    void app.selectedUid;
    queueMicrotask(() => rowsEl?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" }));
  });
  let debounce: ReturnType<typeof setTimeout> | undefined;

  export function focusSearch() {
    searchBox?.focus();
    searchBox?.select();
  }

  function onInput() {
    clearTimeout(debounce);
    debounce = setTimeout(() => void app.runSearch(), 250);
  }

  const title = $derived(app.hits !== null ? `Search results` : (app.folders.find((f) => f.mailbox.name === app.mailbox)?.label ?? app.mailbox));
  const snippet = (r: MessageRow | SearchHit) => ("snippet" in r ? r.snippet : null);

  // In Sent and Drafts the interesting person is who it went to, not you.
  const outgoing = $derived(app.hits === null && ["send", "file"].includes(app.folders.find((f) => f.mailbox.name === app.mailbox)?.icon ?? ""));
  const who = (r: MessageRow | SearchHit) => (outgoing ? `To: ${recipientLabel(r.to_addrs)}` : senderLabel(r));

  // The snippet marks matches with [brackets]; render them as <mark> elements without any HTML.
  function parts(s: string): { text: string; hit: boolean }[] {
    const out: { text: string; hit: boolean }[] = [];
    for (const piece of s.split(/(\[[^\]]*\])/)) {
      if (!piece) continue;
      if (piece.startsWith("[") && piece.endsWith("]")) out.push({ text: piece.slice(1, -1), hit: true });
      else out.push({ text: piece, hit: false });
    }
    return out;
  }
</script>

<section class="list" aria-label="Messages">
  <header>
    <div class="title">
      <h2>{title}</h2>
      {#if app.hits === null && app.currentMailbox?.unseen}<span class="muted">{app.currentMailbox.unseen} unread</span>{/if}
      {#if app.hits !== null}<span class="muted">{app.hits.length} found</span>{/if}
    </div>
    <div class="hdr-actions">
      {#if app.aiEnabled && app.hits === null}
        <button class="icon-btn" onclick={() => app.analyzeFolder()} disabled={app.analysingFolder} title="Analyse the newest messages with AI" aria-label="Analyse folder">
          <Icon name={app.analysingFolder ? "loader" : "sparkles"} />
        </button>
      {/if}
      <button class="icon-btn" onclick={() => app.sync()} disabled={app.syncing} title="Sync this folder" aria-label="Sync">
        <Icon name={app.syncing ? "loader" : "refresh"} />
      </button>
    </div>
  </header>

  <div class="search">
    <Icon name="search" size={14} />
    <input
      bind:this={searchBox}
      bind:value={app.query}
      oninput={onInput}
      onkeydown={(e) => {
        if (e.key === "Escape") {
          app.clearSearch();
          searchBox?.blur();
        }
        if (e.key === "Enter") {
          clearTimeout(debounce);
          void app.runSearch();
        }
      }}
      placeholder="Search mail  (/)"
      aria-label="Search mail"
      spellcheck="false"
    />
    {#if app.query}
      <button class="icon-btn small" onclick={() => app.clearSearch()} aria-label="Clear search"><Icon name="x" size={13} /></button>
    {/if}
  </div>
  {#if app.query}
    <label class="scope"><input type="checkbox" bind:checked={app.searchAll} onchange={() => app.runSearch()} /> Search all accounts</label>
  {/if}

  {#if app.aiEnabled && app.hits === null}
    <div class="filters" role="group" aria-label="Filter messages">
      <button class="pill" class:on={!app.needsReplyOnly} onclick={() => (app.needsReplyOnly = false)}>All</button>
      <button class="pill" class:on={app.needsReplyOnly} onclick={() => (app.needsReplyOnly = true)}>Needs reply{app.needsReplyCount ? ` · ${app.needsReplyCount}` : ""}</button>
    </div>
  {/if}

  <div class="rows" role="listbox" aria-label="Message list" bind:this={rowsEl}>
    {#if app.loadingList && app.rows.length === 0}
      {#each Array(8) as _, i (i)}
        <div class="row ghost"><div class="skeleton" style:width="{40 + ((i * 17) % 40)}%" style:height="12px"></div><div class="skeleton" style:width="{60 + ((i * 11) % 30)}%" style:height="11px"></div></div>
      {/each}
    {:else if app.rows.length === 0}
      <div class="empty">
        <Icon name={app.hits !== null ? "search" : "inbox"} size={28} />
        {#if app.hits !== null}
          <p>No matches for “{app.query}”.</p>
          <p class="muted">Search covers synced headers and message bodies you have opened.</p>
        {:else}
          <p>Nothing here yet.</p>
          <p class="muted">Press sync to fetch this folder.</p>
        {/if}
      </div>
    {:else}
      {#each app.rows as r (r.mailbox_id + ":" + r.uid)}
        {@const unread = isUnread(r)}
        <div
          class="row"
          class:selected={r.uid === app.selectedUid}
          class:unread
          role="option"
          aria-selected={r.uid === app.selectedUid}
          tabindex="-1"
          onclick={() => app.openMessage(r)}
          onkeydown={(e) => e.key === "Enter" && app.openMessage(r)}
        >
          <div class="line1">
            <span class="dot" aria-label={unread ? "Unread" : ""}></span>
            <span class="from">{who(r)}</span>
            {#if r.triage?.suspicious}<span class="ico danger" title="This message tries to instruct an AI"><Icon name="warning" size={13} /></span>{/if}
            {#if r.triage?.needs_reply}<span class="ico reply" title="Needs a reply"><Icon name="reply" size={13} /></span>{/if}
            {#if r.pending_events > 0}<span class="ico event" title="{r.pending_events} possible event{r.pending_events === 1 ? '' : 's'}"><Icon name="calendar" size={13} /></span>{/if}
            {#if r.has_attachments}<span class="clip"><Icon name="clip" size={13} /></span>{/if}
            <button
              class="icon-btn small star"
              class:on={isStarred(r)}
              onclick={(e) => {
                e.stopPropagation();
                void app.setStarred(r.uid, !isStarred(r));
              }}
              aria-label={isStarred(r) ? "Remove star" : "Star"}
              tabindex="-1"><Icon name="star" size={14} fill={isStarred(r)} /></button
            >
            <time>{formatListDate(r.date ?? r.internal_date)}</time>
          </div>
          <div class="subject">{r.subject || "(no subject)"}</div>
          {#if snippet(r)}
            <div class="preview">{#each parts(snippet(r) ?? "") as p}{#if p.hit}<mark>{p.text}</mark>{:else}{p.text}{/if}{/each}</div>
          {:else if r.triage && !r.triage.suspicious}
            <div class="preview"><span class="cat {r.triage.category}">{categoryLabel[r.triage.category]}</span> · {r.triage.summary}</div>
          {:else if r.preview}
            <div class="preview">{r.preview}</div>
          {/if}
        </div>
      {/each}
      {#if app.hasMore}
        <button class="more" onclick={() => app.loadMore()} disabled={app.loadingList}>
          {app.loadingList ? "Loading…" : "Load older messages"}
        </button>
      {/if}
    {/if}
  </div>
</section>

<style>
  .list { display: flex; flex-direction: column; min-height: 0; border-right: 1px solid var(--line); background: var(--bg-0); }
  header { display: flex; align-items: center; justify-content: space-between; padding: 12px 14px 8px; }
  .title { display: flex; align-items: baseline; gap: 10px; min-width: 0; }
  h2 { font-size: 16px; }
  .search {
    display: flex; align-items: center; gap: 8px; margin: 0 12px 8px; padding: 0 10px; color: var(--fg-2);
    background: var(--bg-1); border: 1px solid var(--line-strong); border-radius: var(--radius);
  }
  .search:focus-within { border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-bg); }
  .search input { flex: 1; min-width: 0; background: transparent; border: 0; padding: 8px 0; color: var(--fg-0); outline: none; }
  .scope { display: flex; align-items: center; gap: 6px; margin: -2px 14px 8px; color: var(--fg-2); font-size: 12px; }
  .small { width: 22px; height: 22px; }
  .hdr-actions { display: flex; align-items: center; gap: 2px; }
  .filters { display: flex; gap: 6px; padding: 0 12px 8px; }
  .pill { padding: 2px 11px; background: transparent; border: 1px solid var(--line-strong); border-radius: 999px; color: var(--fg-2); cursor: pointer; font-size: 12.5px; }
  .pill:hover { color: var(--fg-0); }
  .pill.on { background: var(--accent-bg); border-color: var(--accent); color: var(--fg-0); }
  .ico { display: inline-flex; flex: none; }
  .ico.reply { color: var(--accent); }
  .ico.event { color: #bb9af7; }
  .ico.danger { color: var(--danger); }
  .cat { font-weight: 600; color: var(--fg-1); }
  .cat.conversation { color: var(--accent); }
  .cat.scheduling { color: #bb9af7; }
  .cat.transaction { color: var(--warn); }
  .rows { flex: 1; overflow-y: auto; min-height: 0; border-top: 1px solid var(--line); }
  .row {
    position: relative; display: grid; gap: 2px; padding: 10px 14px 10px 26px; border-bottom: 1px solid var(--line);
    cursor: default; content-visibility: auto; contain-intrinsic-size: auto 66px;
  }
  .row:hover { background: var(--bg-1); }
  .row.selected { background: var(--accent-bg); box-shadow: inset 2px 0 0 var(--accent); }
  .row.ghost { gap: 8px; padding: 14px; cursor: default; }
  .line1 { display: flex; align-items: center; gap: 6px; min-width: 0; }
  .dot { position: absolute; left: 10px; top: 17px; width: 7px; height: 7px; border-radius: 50%; background: transparent; }
  .unread .dot { background: var(--accent); }
  .from { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--fg-1); }
  .unread .from, .unread .subject { color: var(--fg-0); font-weight: 600; }
  .clip { color: var(--fg-2); display: inline-flex; }
  .star { opacity: 0; }
  .row:hover .star, .star.on { opacity: 1; }
  time { color: var(--fg-2); font-size: 12px; flex: none; font-variant-numeric: tabular-nums; }
  .subject { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--fg-1); }
  .preview { color: var(--fg-2); font-size: 12.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  mark { background: color-mix(in srgb, var(--warn) 30%, transparent); color: var(--fg-0); border-radius: 3px; padding: 0 1px; }
  .empty { display: grid; justify-items: center; gap: 4px; padding: 48px 24px; color: var(--fg-2); text-align: center; }
  .empty p { margin: 4px 0 0; }
  .more { display: block; width: 100%; padding: 12px; background: transparent; border: 0; color: var(--accent); cursor: pointer; }
  .more:hover:not(:disabled) { background: var(--bg-1); }
</style>
