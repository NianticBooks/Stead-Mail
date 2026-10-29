<script lang="ts">
  import { app } from "../state.svelte";
  import Icon from "./Icon.svelte";
  import { categoryLabel, formatEventTime, isPastEvent } from "../format";
  import { api } from "../api";
  import { cal } from "../calendar.svelte";
  import type { Occurrence } from "../types";

  const t = $derived(app.detail?.triage ?? null);
  const shown = $derived(app.proposals.filter((p) => p.state !== "dismissed"));

  // Tell the user when a suggested event overlaps something already on the calendar.
  let overlaps = $state<Record<number, Occurrence[]>>({});
  $effect(() => {
    const open = shown.filter((p) => p.state === "pending" && p.method !== "CANCEL" && !isPastEvent(p) && !p.all_day);
    let stale = false;
    overlaps = {};
    for (const p of open) {
      api.calendarConflicts(p.start_utc, p.end_utc, undefined).then(
        (c) => { if (!stale && c.length) overlaps = { ...overlaps, [p.id]: c }; },
        () => {},
      );
    }
    return () => (stale = true);
  });

  async function accept(id: number) {
    if (await app.acceptProposal(id)) cal.markStale();
  }
</script>

{#if app.aiEnabled && app.detail}
  <section class="ai" aria-label="AI summary and suggestions">
    {#if t?.suspicious}
      <div class="warn" role="alert">
        <Icon name="warning" size={18} />
        <div>
          <strong>This message tries to give instructions to an AI assistant.</strong>
          <span>It was never shown to an AI model and nothing in it was followed. Treat it with suspicion.</span>
        </div>
      </div>
    {:else if t}
      <div class="summary"><Icon name="sparkles" size={14} /><span>{t.summary}</span></div>
      <div class="chips">
        <span class="chip cat {t.category}">{categoryLabel[t.category] ?? t.category}</span>
        {#if t.needs_reply}<span class="chip reply">Needs reply</span>{/if}
        {#if t.urgency === "high"}<span class="chip urgent">Urgent</span>{/if}
        <span class="spacer"></span>
        <button class="link" onclick={() => app.analyze(true)} disabled={app.analysing} title="Read this message again">{app.analysing ? "Reading…" : "Re-run"}</button>
      </div>
    {:else if app.analysing}
      <div class="summary muted"><Icon name="loader" size={14} /> Reading this message…</div>
    {/if}

    {#if app.aiErrors.length}
      <div class="problem">
        <Icon name="alert" size={14} />
        <span>{app.aiErrors[0]}</span>
        <button class="link" onclick={() => app.analyze(true)} disabled={app.analysing}>Try again</button>
      </div>
    {/if}

    {#each shown as p (p.id)}
      {@const cancelled = p.method === "CANCEL"}
      {@const past = isPastEvent(p)}
      <article class="event" class:done={p.state === "accepted"} class:muted-card={past || cancelled}>
        <div class="top">
          <Icon name="calendar" size={16} />
          <strong class="title">{p.title}</strong>
          {#if cancelled}<span class="badge danger">Cancelled</span>
          {:else if p.source === "ics"}<span class="badge">Invitation</span>
          {:else}<span class="badge warn" title="Read from the text of the message by the local model. Check the details.">Suggested{p.confidence ? ` · ${Math.round(p.confidence * 100)}%` : ""}</span>{/if}
        </div>
        <div class="when">{formatEventTime(p)}{#if p.location} · {p.location}{/if}</div>
        {#if p.source === "ai" && p.quote}<blockquote>“{p.quote}”</blockquote>{/if}
        {#if overlaps[p.id]?.length}
          <div class="overlap" role="status"><Icon name="warning" size={13} /> Overlaps with {overlaps[p.id].slice(0, 2).map((o) => o.title).join(", ")}{overlaps[p.id].length > 2 ? ` and ${overlaps[p.id].length - 2} more` : ""}</div>
        {/if}
        {#if p.attendees.length}<div class="muted small">With {p.attendees.slice(0, 4).join(", ")}{p.attendees.length > 4 ? ` +${p.attendees.length - 4}` : ""}</div>{/if}
        <div class="actions">
          {#if p.state === "accepted"}
            <span class="added"><Icon name="check" size={14} /> Added to your calendar</span>
          {:else}
            {#if !cancelled && !past}<button class="btn sm primary" onclick={() => accept(p.id)}><Icon name="plus" size={14} /> Add to calendar</button>{/if}
            {#if past && !cancelled}<span class="muted small">This date has passed.</span>{/if}
            <button class="btn sm ghost" onclick={() => app.dismissProposal(p.id)}>Dismiss</button>
          {/if}
        </div>
      </article>
    {/each}
  </section>
{/if}

<style>
  .ai { display: grid; grid-template-columns: minmax(0, 1fr); gap: 10px; padding: 12px 14px; background: var(--bg-1); border: 1px solid var(--line); border-radius: var(--radius); }
  .summary { display: flex; gap: 8px; align-items: flex-start; color: var(--fg-0); line-height: 1.5; }
  .summary :global(svg) { margin-top: 3px; color: var(--accent); flex: none; }
  .summary.muted { color: var(--fg-2); }
  .chips { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; }
  .spacer { flex: 1; }
  .chip { padding: 1px 9px; border-radius: 999px; font-size: 12px; font-weight: 600; background: var(--bg-3); color: var(--fg-1); }
  .chip.conversation { background: color-mix(in srgb, var(--accent) 16%, transparent); color: var(--accent); }
  .chip.scheduling { background: color-mix(in srgb, #bb9af7 18%, transparent); color: #bb9af7; }
  .chip.transaction { background: color-mix(in srgb, var(--warn) 16%, transparent); color: var(--warn); }
  .chip.reply { background: var(--accent-strong); color: #0b1020; }
  .chip.urgent { background: color-mix(in srgb, var(--danger) 18%, transparent); color: var(--danger); }
  .link { background: none; border: 0; color: var(--fg-2); cursor: pointer; padding: 0; font-size: 12.5px; }
  .link:hover:not(:disabled) { color: var(--accent); text-decoration: underline; }
  .warn {
    display: flex; gap: 12px; align-items: flex-start; padding: 10px 12px; border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--danger) 12%, transparent); border: 1px solid color-mix(in srgb, var(--danger) 40%, transparent); color: var(--fg-0);
  }
  .warn :global(svg) { color: var(--danger); flex: none; margin-top: 1px; }
  .warn div { display: grid; gap: 2px; }
  .warn span { color: var(--fg-1); font-size: 12.5px; }
  .problem { display: flex; gap: 8px; align-items: center; color: var(--fg-2); font-size: 12.5px; }
  .problem span { flex: 1; min-width: 0; overflow-wrap: anywhere; }
  .event { display: grid; gap: 6px; padding: 10px 12px; background: var(--bg-2); border: 1px solid var(--line-strong); border-radius: var(--radius-sm); }
  .event.done { border-color: color-mix(in srgb, var(--ok) 45%, transparent); }
  .event.muted-card { opacity: 0.7; }
  .top { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .top .badge { flex: none; white-space: nowrap; align-self: center; line-height: 18px; }
  .top :global(svg) { color: var(--accent); flex: none; }
  .title { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .when { color: var(--fg-1); }
  blockquote { margin: 0; padding-left: 10px; border-left: 2px solid var(--line-strong); color: var(--fg-2); font-size: 12.5px; font-style: italic; overflow-wrap: anywhere; display: -webkit-box; -webkit-line-clamp: 3; line-clamp: 3; -webkit-box-orient: vertical; overflow: hidden; }
  .actions { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .added { display: inline-flex; gap: 6px; align-items: center; color: var(--ok); font-size: 12.5px; }
  .small { font-size: 12.5px; }
  .overlap { display: flex; gap: 6px; align-items: center; color: var(--warn); font-size: 12.5px; }
</style>
