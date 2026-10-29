<script lang="ts">
  import { autofocus } from "../focus";
  import { api } from "../api";
  import { cal } from "../calendar.svelte";
  import {
    addDays, browserTz, dayKey, sameDay, describeRule, kindOf, parseDayKey, ruleFor, toDateInput, toLocalInput, zonedToUtc, formatWhen, type RepeatKind,
  } from "../cal";
  import type { CalEvent, EventInput, Occurrence } from "../types";
  import Icon from "./Icon.svelte";

  const pad = (n: number) => String(n).padStart(2, "0");
  const localStamp = (d: Date) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
  const minutesBetween = (a: string, b: string) => Math.round((new Date(b).getTime() - new Date(a).getTime()) / 60000);

  interface Form {
    title: string;
    allDay: boolean;
    tz: string;
    startDate: string;
    startTime: string;
    endDate: string;
    endTime: string;
    repeat: RepeatKind;
    custom: string;
    location: string;
    notes: string;
    calendarId: string;
    attendees: string;
    status: string;
  }

  function makeForm(): Form {
    const d = cal.dialog!;
    const ev: CalEvent | undefined = d.event;
    if (ev) {
      const tz = ev.tz || browserTz();
      const s = toLocalInput(ev.start_utc, tz);
      const e = toLocalInput(ev.end_utc, tz);
      const lastDay = ev.all_day ? dayKey(addDays(parseDayKey(e.slice(0, 10)), -1)) : e.slice(0, 10);
      return {
        title: ev.title, allDay: ev.all_day, tz, startDate: s.slice(0, 10), startTime: ev.all_day ? "09:00" : s.slice(11),
        endDate: lastDay, endTime: ev.all_day ? "10:00" : e.slice(11),
        repeat: kindOf(ev.rrule, parseDayKey(s.slice(0, 10))), custom: ev.rrule ?? "", location: ev.location ?? "", notes: ev.description ?? "",
        calendarId: ev.calendar_id, attendees: ev.attendees.join(", "), status: ev.status,
      };
    }
    let start: string;
    if (d.start) start = d.start.length === 10 ? `${d.start}T09:00` : d.start;
    else {
      // On today, the next half hour. On another day being viewed, 9 AM that day.
      const n = new Date();
      if (sameDay(cal.anchor, n)) {
        n.setMinutes(n.getMinutes() < 30 ? 30 : 60, 0, 0);
        start = localStamp(n);
      } else {
        start = `${dayKey(cal.anchor)}T09:00`;
      }
    }
    const end = localStamp(new Date(new Date(start).getTime() + 3600_000));
    return {
      title: "", allDay: !!d.allDay, tz: browserTz(), startDate: start.slice(0, 10), startTime: start.slice(11),
      endDate: d.allDay ? start.slice(0, 10) : end.slice(0, 10), endTime: end.slice(11), repeat: "none", custom: "", location: "", notes: "",
      calendarId: cal.writable[0]?.id ?? "", attendees: "", status: "confirmed",
    };
  }

  let f = $state<Form>(makeForm());
  /** Minutes before the start; empty means no reminder. Kept on this machine, never synced. */
  let reminders = $state<number[]>([]);
  const LEADS: [number, string][] = [[0, "At start"], [5, "5 min"], [10, "10 min"], [15, "15 min"], [30, "30 min"], [60, "1 hour"], [1440, "1 day"]];
  function toggleLead(m: number) {
    reminders = reminders.includes(m) ? reminders.filter((x) => x !== m) : [...reminders, m].sort((a, b) => a - b);
  }
  $effect(() => {
    const id = eventId;
    if (id) void api.eventReminders(id).then((r) => (reminders = r)).catch(() => {});
  });
  let busy = $state(false);
  let error = $state<string | null>(null);
  let confirmDelete = $state(false);
  let conflicts = $state<Occurrence[]>([]);

  const editing = $derived(cal.dialog?.mode === "edit");
  const eventId = $derived(cal.dialog?.event?.id);
  const occurrence = $derived(cal.dialog?.occurrence);
  const recurring = $derived(!!cal.dialog?.event?.rrule);
  const readOnly = $derived(cal.calendars.find((c) => c.id === f.calendarId)?.read_only ?? false);
  const rule = $derived(ruleFor(f.repeat, parseDayKey(f.startDate || dayKey(new Date())), f.custom));
  const startStamp = $derived(f.allDay ? f.startDate : `${f.startDate}T${f.startTime}`);
  const endStamp = $derived(f.allDay ? f.endDate : `${f.endDate}T${f.endTime}`);
  const endsBeforeStart = $derived(f.allDay ? f.endDate < f.startDate : endStamp <= startStamp);
  const canSave = $derived(f.title.trim() !== "" && !!f.startDate && !!f.endDate && !!f.startTime && !!f.endTime && !endsBeforeStart && !busy && !!f.calendarId);

  // Keep the end the same distance after the start when the start moves.
  let prevStart = `${f.startDate}T${f.startTime}`;
  function startChanged() {
    const now = `${f.startDate}T${f.startTime}`;
    if (f.allDay) {
      if (f.endDate < f.startDate) f.endDate = f.startDate;
    } else if (f.startDate && f.startTime && prevStart.length >= 16) {
      const dur = minutesBetween(prevStart, `${f.endDate}T${f.endTime}`);
      if (dur > 0) {
        const e = localStamp(new Date(new Date(now).getTime() + dur * 60000));
        f.endDate = e.slice(0, 10);
        f.endTime = e.slice(11);
      }
    }
    prevStart = now;
  }

  function toggleAllDay() {
    if (f.allDay && f.endDate < f.startDate) f.endDate = f.startDate;
    if (!f.allDay && f.endDate === f.startDate && f.endTime <= f.startTime) {
      const e = localStamp(new Date(new Date(`${f.startDate}T${f.startTime}`).getTime() + 3600_000));
      f.endDate = e.slice(0, 10);
      f.endTime = e.slice(11);
    }
  }

  // Warn (never block) when the time overlaps something else.
  $effect(() => {
    const [s, e, all, tz, id] = [startStamp, endStamp, f.allDay, f.tz, eventId];
    conflicts = [];
    if (all || endsBeforeStart) return;
    const a = zonedToUtc(s, tz);
    const b = zonedToUtc(e, tz);
    if (!a || !b) return;
    const timer = setTimeout(() => {
      api.calendarConflicts(a.toISOString(), b.toISOString(), id).then((c) => (conflicts = c), () => (conflicts = []));
    }, 250);
    return () => clearTimeout(timer);
  });

  function input(): EventInput {
    return {
      calendar_id: f.calendarId,
      title: f.title,
      description: f.notes,
      location: f.location,
      start: startStamp,
      end: endStamp,
      all_day: f.allDay,
      tz: f.tz.trim() || browserTz(),
      rrule: rule,
      status: f.status,
      attendees: f.attendees.split(/[\s,;]+/).filter(Boolean),
    };
  }

  async function save() {
    if (!canSave) return;
    busy = true;
    error = null;
    error = await cal.saveEvent(input(), eventId, reminders);
    busy = false;
  }

  async function remove(instance?: string) {
    if (!eventId) return;
    busy = true;
    error = null;
    error = await cal.deleteEvent(eventId, instance);
    busy = false;
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      cal.closeDialog();
    } else if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      void save();
    }
  }

  const zones: string[] = (() => {
    try {
      return (Intl as unknown as { supportedValuesOf(k: string): string[] }).supportedValuesOf("timeZone");
    } catch {
      return [];
    }
  })();
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div class="overlay" role="dialog" aria-modal="true" aria-label={editing ? "Edit event" : "New event"} tabindex="-1" use:autofocus={"input[name=title]"} {onkeydown}>
  <form class="modal" onsubmit={(e) => { e.preventDefault(); void save(); }}>
    <header>
      <h2>{editing ? "Edit event" : "New event"}</h2>
      <span class="spacer"></span>
      <button type="button" class="icon-btn" onclick={() => cal.closeDialog()} aria-label="Close"><Icon name="x" /></button>
    </header>

    <div class="body">
      <input class="input title" name="title" placeholder="Title" bind:value={f.title} maxlength="200" autocomplete="off" />

      <div class="when">
        <label class="check"><input type="checkbox" bind:checked={f.allDay} onchange={toggleAllDay} /> All day</label>
        <div class="dt">
          <span class="lbl">Start</span>
          <input class="input" type="date" bind:value={f.startDate} oninput={startChanged} aria-label="Start date" />
          {#if !f.allDay}<input class="input time" type="time" bind:value={f.startTime} oninput={startChanged} aria-label="Start time" />{/if}
        </div>
        <div class="dt">
          <span class="lbl">End</span>
          <input class="input" type="date" bind:value={f.endDate} aria-label="End date" />
          {#if !f.allDay}<input class="input time" type="time" bind:value={f.endTime} aria-label="End time" />{/if}
        </div>
        {#if endsBeforeStart}<div class="problem">The event must end after it starts.</div>{/if}
        <div class="row">
          <label class="field grow">Time zone
            <input class="input" list="zones" bind:value={f.tz} spellcheck="false" />
            <datalist id="zones">{#each zones as z}<option value={z}></option>{/each}</datalist>
          </label>
          <label class="field grow">Repeat
            <select class="input" bind:value={f.repeat}>
              <option value="none">Does not repeat</option>
              <option value="daily">Every day</option>
              <option value="weekdays">Every weekday</option>
              <option value="weekly">Every week</option>
              <option value="monthly">Every month</option>
              <option value="yearly">Every year</option>
              <option value="custom">Custom rule…</option>
            </select>
          </label>
        </div>
        {#if f.repeat === "custom"}
          <label class="field">Repeat rule
            <input class="input mono" bind:value={f.custom} placeholder="FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE" spellcheck="false" />
          </label>
        {/if}
        {#if rule}<div class="muted small"><Icon name="refresh" size={12} /> {describeRule(rule)}{editing && recurring ? " · changes apply to the whole series" : ""}</div>{/if}
      </div>

      {#if conflicts.length}
        <div class="conflict" role="status">
          <Icon name="warning" size={15} />
          <div>
            <strong>Overlaps with {conflicts.length === 1 ? "another event" : `${conflicts.length} other events`}</strong>
            <span>{conflicts.slice(0, 3).map((c) => c.title).join(", ")}{conflicts.length > 3 ? ` and ${conflicts.length - 3} more` : ""}. You can still save it.</span>
          </div>
        </div>
      {/if}

      <div class="field">Remind me
        <div class="leads">
          {#each LEADS as [m, label] (m)}
            <label class="check"><input type="checkbox" checked={reminders.includes(m)} onchange={() => toggleLead(m)} /> {label}</label>
          {/each}
        </div>
      </div>
      <label class="field">Location <input class="input" bind:value={f.location} maxlength="300" autocomplete="off" /></label>
      <label class="field">Notes <textarea class="input" rows="3" bind:value={f.notes}></textarea></label>
      <label class="field">Guests <input class="input" bind:value={f.attendees} placeholder="name@example.com, …" autocomplete="off" /></label>
      <div class="row">
        <label class="field grow">Calendar
          <select class="input" bind:value={f.calendarId} disabled={editing && cal.calendars.some((c) => c.kind !== "local")} title={editing && cal.calendars.some((c) => c.kind !== "local") ? "An event stays in its calendar once it exists" : ""}>
            {#each cal.writable as c (c.id)}<option value={c.id}>{c.name}</option>{/each}
          </select>
        </label>
        <label class="field grow">Status
          <select class="input" bind:value={f.status}>
            <option value="confirmed">Confirmed</option>
            <option value="tentative">Tentative</option>
            <option value="cancelled">Cancelled</option>
          </select>
        </label>
      </div>
      {#if occurrence && recurring}<div class="muted small">You opened the occurrence on {formatWhen(occurrence)}.</div>{/if}
      {#if error}<div class="problem" role="alert">{error}</div>{/if}
    </div>

    <footer>
      {#if editing}
        {#if confirmDelete}
          <span class="muted small">{recurring ? "Delete every occurrence?" : "Delete this event?"}</span>
          <button type="button" class="btn sm danger" onclick={() => remove()} disabled={busy}>Yes, delete</button>
          <button type="button" class="btn sm ghost" onclick={() => (confirmDelete = false)}>Keep</button>
        {:else}
          {#if recurring && occurrence}<button type="button" class="btn sm" onclick={() => remove(occurrence.instance)} disabled={busy || readOnly}>Delete this one</button>{/if}
          <button type="button" class="btn sm danger" onclick={() => (confirmDelete = true)} disabled={busy || readOnly}><Icon name="trash" size={14} /> {recurring ? "Delete series" : "Delete"}</button>
        {/if}
      {/if}
      <span class="spacer"></span>
      <button type="button" class="btn" onclick={() => cal.closeDialog()}>Cancel</button>
      <button type="submit" class="btn primary" disabled={!canSave || readOnly}>{busy ? "Saving…" : "Save"}</button>
    </footer>
  </form>
</div>

<style>
  .modal { width: min(560px, 100%); }
  header { display: flex; align-items: center; gap: 8px; padding: 12px 16px; border-bottom: 1px solid var(--line); }
  .spacer { flex: 1; }
  .body { overflow-y: auto; padding: 14px 16px; display: grid; gap: 12px; min-height: 0; }
  .title { font-size: 16px; padding: 9px 11px; }
  .when { display: grid; gap: 8px; }
  .dt { display: flex; gap: 8px; align-items: center; }
  .dt .input { flex: 1; min-width: 0; }
  .dt .input.time { flex: 0 0 140px; }
  .lbl { width: 40px; color: var(--fg-2); font-size: 12.5px; }
  .row { display: flex; gap: 8px; align-items: flex-end; flex-wrap: wrap; }
  .grow { flex: 1; min-width: 150px; }
  .check { display: inline-flex; gap: 8px; align-items: center; cursor: pointer; width: fit-content; }
  .mono { font-family: var(--font-mono); font-size: 12.5px; }
  textarea.input { resize: vertical; min-height: 60px; }
  .small { font-size: 12.5px; display: flex; gap: 6px; align-items: center; }
  .problem { color: var(--danger); font-size: 12.5px; overflow-wrap: anywhere; }
  .conflict {
    display: flex; gap: 10px; align-items: flex-start; padding: 8px 12px; border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--warn) 12%, transparent); border: 1px solid color-mix(in srgb, var(--warn) 40%, transparent);
  }
  .conflict :global(svg) { color: var(--warn); flex: none; margin-top: 2px; }
  .conflict div { display: grid; gap: 1px; }
  .conflict span { color: var(--fg-1); font-size: 12.5px; }
  footer { display: flex; gap: 8px; align-items: center; padding: 10px 16px; border-top: 1px solid var(--line); }
  .leads { display: flex; flex-wrap: wrap; gap: 4px 14px; }
</style>
