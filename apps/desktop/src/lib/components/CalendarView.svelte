<script lang="ts">
  import { onMount, tick } from "svelte";
  import { cal } from "../calendar.svelte";
  import {
    daysIn, dayKey, formatClock, formatHour, formatWhen, layoutColumns, sameDay, segmentsByDay, startOfDay, titleFor, describeRule, type CalView, type Segment,
  } from "../cal";
  import Icon from "./Icon.svelte";

  const HOUR_PX = 48;
  const views: { id: CalView; label: string; key: string }[] = [
    { id: "month", label: "Month", key: "m" },
    { id: "week", label: "Week", key: "w" },
    { id: "day", label: "Day", key: "d" },
    { id: "agenda", label: "Agenda", key: "a" },
  ];

  let now = $state(new Date());
  let scroller = $state<HTMLElement | null>(null);

  const days = $derived(daysIn(cal.range.from, cal.range.to));
  const segs = $derived(segmentsByDay(cal.occurrences, days));
  const title = $derived(titleFor(cal.view, cal.anchor, cal.weekStart));
  const nowMin = $derived(now.getHours() * 60 + now.getMinutes());
  const hours = Array.from({ length: 23 }, (_, i) => i + 1);
  const monthOf = $derived(cal.anchor.getMonth());
  const agendaDays = $derived(days.filter((d) => (segs.get(dayKey(d)) ?? []).length > 0));

  const weekday = (d: Date, style: "short" | "long" = "short") => new Intl.DateTimeFormat(undefined, { weekday: style }).format(d);

  onMount(() => {
    const t = setInterval(() => (now = new Date()), 30_000);
    return () => clearInterval(t);
  });

  // Start the time grid around the working day, or at the current time if it is today.
  $effect(() => {
    if ((cal.view !== "week" && cal.view !== "day") || !scroller) return;
    const todayShown = days.some((d) => sameDay(d, new Date()));
    const hour = todayShown ? Math.max(0, new Date().getHours() - 2) : 7;
    void tick().then(() => scroller && (scroller.scrollTop = hour * HOUR_PX));
  });

  function openSeg(s: Segment) {
    void cal.openOccurrence(s.occ);
  }

  function newAt(d: Date, minutes: number) {
    const m = Math.max(0, Math.min(23 * 60 + 30, Math.round(minutes / 30) * 30));
    cal.openNew(`${dayKey(d)}T${String(Math.floor(m / 60)).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`);
  }

  function columnDblclick(e: MouseEvent, d: Date) {
    if (e.target !== e.currentTarget) return;
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    newAt(d, ((e.clientY - rect.top) / HOUR_PX) * 60);
  }

  const style = (color: string) => `--c: ${color}`;
  const label = (s: Segment) => `${s.occ.title}, ${s.banner ? "all day" : formatClock(s.occ.start_utc)}`;
  const timeText = (s: Segment) => (s.banner ? "" : formatClock(s.occ.start_utc));
</script>

<section class="cal" aria-label="Calendar">
  <header class="bar">
    <button class="btn sm" onclick={() => cal.today()}>Today</button>
    <button class="icon-btn prev" onclick={() => cal.step(-1)} aria-label="Previous" title="Previous ( [ )"><Icon name="chevron" /></button>
    <button class="icon-btn next" onclick={() => cal.step(1)} aria-label="Next" title="Next ( ] )"><Icon name="chevron" /></button>
    <h1>{title}</h1>
    {#if cal.loading}<span class="muted"><Icon name="loader" size={14} /></span>{/if}
    <span class="spacer"></span>
    <div class="seg" role="tablist" aria-label="View">
      {#each views as v}
        <button role="tab" aria-selected={cal.view === v.id} class:on={cal.view === v.id} onclick={() => cal.setView(v.id)} title="{v.label} ({v.key})">{v.label}</button>
      {/each}
    </div>
    <button class="btn primary sm" onclick={() => cal.openNew()} title="New event (n)"><Icon name="plus" size={14} /> New event</button>
  </header>

  {#if cal.error}
    <div class="error" role="alert"><Icon name="alert" size={14} /> {cal.error} <button class="link" onclick={() => cal.refresh()}>Try again</button></div>
  {/if}

  {#if cal.view === "month"}
    <div class="month">
      <div class="dow">{#each days.slice(0, 7) as d}<span>{weekday(d)}</span>{/each}</div>
      <div class="cells">
        {#each days as d (dayKey(d))}
          {@const list = segs.get(dayKey(d)) ?? []}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="cell" class:other={d.getMonth() !== monthOf} class:today={sameDay(d, now)} ondblclick={(e) => e.target === e.currentTarget && cal.openNew(dayKey(d))}>
            <button class="num" onclick={() => cal.goToDay(d)} aria-label={new Intl.DateTimeFormat(undefined, { dateStyle: "full" }).format(d)}>{d.getDate() === 1 ? new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(d) : d.getDate()}</button>
            {#each list.slice(0, 3) as s (s.occ.event_id + s.occ.instance + s.day)}
              <button class="chip" class:banner={s.banner} class:cancelled={s.occ.status === "cancelled"} class:tentative={s.occ.status === "tentative"} style={style(s.occ.color)} onclick={() => openSeg(s)} aria-label={label(s)}>
                {#if !s.banner}<span class="t">{timeText(s)}</span>{/if}<span class="n">{s.occ.title}</span>
              </button>
            {/each}
            {#if list.length > 3}<button class="more" onclick={() => cal.goToDay(d)}>+{list.length - 3} more</button>{/if}
          </div>
        {/each}
      </div>
    </div>
  {:else if cal.view === "week" || cal.view === "day"}
    <div class="scroll" bind:this={scroller}>
      <div class="head" style:--n={days.length}>
        <div class="gutter"></div>
        {#each days as d (dayKey(d))}
          {@const banners = (segs.get(dayKey(d)) ?? []).filter((s) => s.banner)}
          <div class="dayhead" class:today={sameDay(d, now)}>
            <button class="dh" onclick={() => cal.goToDay(d)}><span class="wd">{weekday(d)}</span> <span class="dn">{d.getDate()}</span></button>
            {#each banners as s (s.occ.event_id + s.occ.instance)}
              <button class="chip banner" class:cancelled={s.occ.status === "cancelled"} style={style(s.occ.color)} onclick={() => openSeg(s)} aria-label={label(s)}>
                <span class="n">{s.occ.title}</span>
              </button>
            {/each}
          </div>
        {/each}
      </div>
      <div class="grid" style:--n={days.length} style:--h="{HOUR_PX}px">
        <div class="gutter">
          {#each hours as h}<span class="hour" style:top="{h * HOUR_PX}px">{formatHour(h)}</span>{/each}
        </div>
        {#each days as d (dayKey(d))}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="col" class:today={sameDay(d, now)} ondblclick={(e) => columnDblclick(e, d)}>
            {#each layoutColumns(segs.get(dayKey(d)) ?? []) as p (p.seg.occ.event_id + p.seg.occ.instance + p.seg.day)}
              {@const s = p.seg}
              <button
                class="ev" class:cancelled={s.occ.status === "cancelled"} class:tentative={s.occ.status === "tentative"}
                style="{style(s.occ.color)}; top: {(s.startMin / 60) * HOUR_PX}px; height: {Math.max(((s.endMin - s.startMin) / 60) * HOUR_PX - 2, 18)}px; left: calc({(p.col / p.cols) * 100}% + 1px); width: calc({100 / p.cols}% - 3px)"
                onclick={() => openSeg(s)} aria-label={label(s)}
              >
                <span class="n">{s.occ.title}</span>
                {#if (s.endMin - s.startMin) >= 45}<span class="t">{formatClock(s.occ.start_utc)}{s.occ.location ? ` · ${s.occ.location}` : ""}</span>{/if}
              </button>
            {/each}
            {#if sameDay(d, now)}<div class="now" style:top="{(nowMin / 60) * HOUR_PX}px"></div>{/if}
          </div>
        {/each}
      </div>
    </div>
  {:else}
    <div class="agenda">
      {#each agendaDays as d (dayKey(d))}
        <div class="aday">
          <h3 class:today={sameDay(d, now)}>{new Intl.DateTimeFormat(undefined, { weekday: "long", month: "long", day: "numeric" }).format(d)}{sameDay(d, now) ? " · Today" : ""}</h3>
          {#each segs.get(dayKey(d)) ?? [] as s (s.occ.event_id + s.occ.instance + s.day)}
            <button class="arow" class:cancelled={s.occ.status === "cancelled"} onclick={() => openSeg(s)}>
              <span class="when">{s.banner ? "All day" : `${formatClock(s.occ.start_utc)} – ${formatClock(s.occ.end_utc)}`}</span>
              <span class="dot" style={style(s.occ.color)}></span>
              <span class="what"><strong>{s.occ.title}</strong>{#if s.occ.location}<span class="muted"> · {s.occ.location}</span>{/if}</span>
              {#if s.occ.recurring}<span class="muted rep" title={formatWhen(s.occ)}><Icon name="refresh" size={12} /></span>{/if}
            </button>
          {/each}
        </div>
      {:else}
        <div class="empty"><Icon name="calendar" size={28} /><p>{cal.loading ? "Loading…" : "Nothing scheduled in this period."}</p></div>
      {/each}
    </div>
  {/if}
</section>

<style>
  .cal { display: flex; flex-direction: column; min-width: 0; min-height: 0; height: 100vh; background: var(--bg-0); }
  .bar { display: flex; align-items: center; gap: 8px; padding: 10px 16px; border-bottom: 1px solid var(--line); }
  .bar h1 { font-size: 16px; margin-left: 8px; white-space: nowrap; }
  .spacer { flex: 1; }
  .prev :global(svg) { transform: rotate(90deg); }
  .next :global(svg) { transform: rotate(-90deg); }
  .seg { display: inline-flex; background: var(--bg-2); border: 1px solid var(--line-strong); border-radius: var(--radius-sm); padding: 2px; }
  .seg button { border: 0; background: transparent; color: var(--fg-1); padding: 3px 10px; border-radius: 4px; cursor: pointer; }
  .seg button:hover { color: var(--fg-0); }
  .seg button.on { background: var(--accent-bg); color: var(--accent); font-weight: 600; }
  .error { display: flex; gap: 8px; align-items: center; padding: 8px 16px; color: var(--danger); border-bottom: 1px solid var(--line); }
  .link { background: none; border: 0; color: var(--fg-1); cursor: pointer; text-decoration: underline; padding: 0; }

  /* chips are used by month cells and banners */
  .chip {
    display: flex; gap: 5px; align-items: center; width: 100%; min-width: 0; text-align: left; padding: 1px 6px; border: 0; border-radius: 4px;
    background: transparent; color: var(--fg-0); cursor: pointer; font-size: 12px; line-height: 18px; box-shadow: inset 3px 0 0 var(--c); padding-left: 9px;
  }
  .chip:hover { background: color-mix(in srgb, var(--c) 18%, transparent); }
  .chip.banner { background: color-mix(in srgb, var(--c) 28%, transparent); }
  .chip.banner:hover { background: color-mix(in srgb, var(--c) 40%, transparent); }
  .chip .n { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .chip .t { color: var(--fg-2); flex: none; font-variant-numeric: tabular-nums; }
  .cancelled .n { text-decoration: line-through; opacity: 0.6; }

  /* month */
  .month { flex: 1; display: flex; flex-direction: column; min-height: 0; }
  .dow { display: grid; grid-template-columns: repeat(7, 1fr); padding: 6px 0; border-bottom: 1px solid var(--line); color: var(--fg-2); font-size: 12px; text-align: center; text-transform: uppercase; letter-spacing: 0.04em; }
  .cells { flex: 1; display: grid; grid-template-columns: repeat(7, minmax(0, 1fr)); grid-template-rows: repeat(6, minmax(0, 1fr)); min-height: 0; }
  .cell { border-right: 1px solid var(--line); border-bottom: 1px solid var(--line); padding: 2px 3px; display: flex; flex-direction: column; gap: 1px; min-height: 0; overflow: hidden; }
  .cell:nth-child(7n) { border-right: 0; }
  .cell.other { background: color-mix(in srgb, var(--bg-1) 55%, transparent); }
  .cell.other .num { color: var(--fg-3); }
  .num { align-self: flex-start; min-width: 24px; height: 22px; padding: 0 5px; border: 0; border-radius: 11px; background: transparent; color: var(--fg-1); cursor: pointer; font-size: 12px; }
  .num:hover { background: var(--bg-3); }
  .cell.today .num { background: var(--accent-strong); color: #0b1020; font-weight: 700; }
  .more { align-self: flex-start; border: 0; background: none; color: var(--fg-2); font-size: 11.5px; cursor: pointer; padding: 0 6px; }
  .more:hover { color: var(--accent); }

  /* week / day */
  .scroll { flex: 1; overflow-y: auto; min-height: 0; position: relative; }
  .head { position: sticky; top: 0; z-index: 3; display: grid; grid-template-columns: 56px repeat(var(--n), minmax(0, 1fr)); background: var(--bg-0); border-bottom: 1px solid var(--line-strong); }
  .dayhead { border-left: 1px solid var(--line); padding: 4px 3px 6px; display: grid; gap: 2px; align-content: start; min-width: 0; }
  .dh { border: 0; background: none; cursor: pointer; color: var(--fg-1); display: flex; gap: 6px; align-items: baseline; justify-content: center; padding: 2px; border-radius: 6px; }
  .dh:hover { background: var(--bg-2); }
  .wd { font-size: 12px; text-transform: uppercase; letter-spacing: 0.04em; color: var(--fg-2); }
  .dn { font-size: 18px; font-weight: 600; min-width: 30px; text-align: center; border-radius: 15px; }
  .dayhead.today .dn { background: var(--accent-strong); color: #0b1020; }
  .dayhead.today .wd { color: var(--accent); }
  .grid { display: grid; grid-template-columns: 56px repeat(var(--n), minmax(0, 1fr)); height: calc(24 * var(--h)); position: relative; }
  .gutter { position: relative; }
  .hour { position: absolute; right: 8px; transform: translateY(-50%); font-size: 11px; color: var(--fg-3); font-variant-numeric: tabular-nums; }
  .col { position: relative; border-left: 1px solid var(--line); background-image: repeating-linear-gradient(to bottom, transparent 0, transparent calc(var(--h) - 1px), var(--line) calc(var(--h) - 1px), var(--line) var(--h)); }
  .col.today { background-color: color-mix(in srgb, var(--accent) 4%, transparent); }
  .ev {
    position: absolute; display: grid; align-content: start; gap: 0; padding: 2px 6px; overflow: hidden; text-align: left; cursor: pointer;
    border: 0; box-shadow: inset 3px 0 0 var(--c); padding-left: 9px; border-radius: 4px; background: color-mix(in srgb, var(--c) 24%, var(--bg-1)); color: var(--fg-0); font-size: 12px; line-height: 1.3;
  }
  .ev:hover { background: color-mix(in srgb, var(--c) 36%, var(--bg-1)); z-index: 2; }
  .ev.tentative { background: repeating-linear-gradient(135deg, color-mix(in srgb, var(--c) 18%, var(--bg-1)) 0 6px, color-mix(in srgb, var(--c) 8%, var(--bg-1)) 6px 12px); }
  .ev .n { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ev .t { color: var(--fg-1); font-size: 11px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .now { position: absolute; left: 0; right: 0; height: 2px; background: var(--danger); z-index: 2; pointer-events: none; }
  .now::before { content: ""; position: absolute; left: -4px; top: -3px; width: 8px; height: 8px; border-radius: 50%; background: var(--danger); }

  /* agenda */
  .agenda { flex: 1; overflow-y: auto; padding: 8px 16px 24px; min-height: 0; }
  .aday { max-width: 760px; margin: 0 auto; }
  .aday h3 { font-size: 13px; color: var(--fg-1); padding: 14px 4px 6px; border-bottom: 1px solid var(--line); }
  .aday h3.today { color: var(--accent); }
  .arow { display: grid; grid-template-columns: 150px 10px minmax(0, 1fr) auto; gap: 10px; align-items: center; width: 100%; text-align: left; padding: 8px 6px; border: 0; background: transparent; color: var(--fg-0); cursor: pointer; border-radius: 6px; }
  .arow:hover { background: var(--bg-2); }
  .arow .when { color: var(--fg-2); font-variant-numeric: tabular-nums; font-size: 12.5px; }
  .arow .dot { width: 10px; height: 10px; border-radius: 50%; background: var(--c); }
  .arow .what { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .arow.cancelled .what { text-decoration: line-through; opacity: 0.6; }
  .empty { display: grid; justify-items: center; gap: 6px; padding: 80px 20px; color: var(--fg-2); }
</style>
