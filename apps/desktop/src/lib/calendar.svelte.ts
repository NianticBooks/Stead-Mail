import { api, shell } from "./api";
import { app } from "./state.svelte";
import { open as pickFiles } from "@tauri-apps/plugin-dialog";
import { addDays, browserTz, rangeFor, shift, startOfDay, type CalView, type WeekStart } from "./cal";
import type { CalEvent, CalSyncReport, Calendar, EventInput, Occurrence, RemoteList, SyncLogEntry } from "./types";

/** How often linked calendars sync while the app is open. */
const AUTO_SYNC_MS = 5 * 60 * 1000;

const PREFS_KEY = "stead.calendar";

interface CalPrefs {
  view: CalView;
  weekStart: WeekStart;
}

function loadPrefs(): CalPrefs {
  try {
    const p = JSON.parse(localStorage.getItem(PREFS_KEY) ?? "{}");
    return {
      view: ["month", "week", "day", "agenda"].includes(p.view) ? p.view : "week",
      weekStart: p.weekStart === 1 ? 1 : 0,
    };
  } catch {
    return { view: "week", weekStart: 0 };
  }
}

const errText = (e: unknown) => (typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e));

/** What the event dialog opens with. */
export interface DialogState {
  mode: "new" | "edit";
  event?: CalEvent;
  /** Local start for a new event, "YYYY-MM-DDTHH:MM", or "YYYY-MM-DD" for all-day. */
  start?: string;
  allDay?: boolean;
  /** The occurrence that was clicked, so one occurrence of a series can be removed. */
  occurrence?: Occurrence;
}

class CalendarState {
  #prefs = loadPrefs();
  view = $state<CalView>(this.#prefs.view);
  weekStart = $state<WeekStart>(this.#prefs.weekStart);
  anchor = $state(startOfDay(new Date()));
  calendars = $state<Calendar[]>([]);
  occurrences = $state<Occurrence[]>([]);
  loading = $state(false);
  error = $state<string | null>(null);
  dialog = $state<DialogState | null>(null);
  calendarDialog = $state<Calendar | "new" | null>(null);
  stale = true;
  #token = 0;

  // Two-way sync with Google and iCloud
  syncing = $state(false);
  connectOpen = $state(false);
  logOpen = $state(false);
  remote = $state<RemoteList | null>(null);
  remoteLoading = $state(false);
  log = $state<SyncLogEntry[]>([]);
  #timer: ReturnType<typeof setInterval> | null = null;

  get range() {
    return rangeFor(this.view, this.anchor, this.weekStart);
  }

  get overlayOpen() {
    return this.dialog !== null || this.calendarDialog !== null || this.connectOpen || this.logOpen;
  }

  #save() {
    try {
      localStorage.setItem(PREFS_KEY, JSON.stringify({ view: this.view, weekStart: this.weekStart }));
    } catch {
      /* preferences are a convenience */
    }
  }

  /** Something outside changed the calendar (a proposal was accepted): reload next time it is shown. */
  markStale() {
    this.stale = true;
    if (app.view === "calendar") void this.refresh();
  }

  async open() {
    const first = this.calendars.length === 0;
    if (first) await this.loadCalendars();
    if (this.stale) await this.refresh();
    this.startAutoSync();
    // Entering the calendar for the first time this session catches up with the servers.
    if (first && this.hasSynced) void this.sync(undefined, true);
  }

  async loadCalendars() {
    try {
      this.calendars = await api.calendars();
    } catch (e) {
      this.error = errText(e);
    }
  }

  /** Load what the current view shows. A newer request supersedes an older one. */
  async refresh() {
    const token = ++this.#token;
    this.loading = true;
    const { from, to } = this.range;
    try {
      const occ = await api.calendarRange(from.toISOString(), to.toISOString());
      if (token !== this.#token) return;
      this.occurrences = occ;
      this.error = null;
      this.stale = false;
    } catch (e) {
      if (token === this.#token) this.error = errText(e);
    } finally {
      if (token === this.#token) this.loading = false;
    }
  }

  setView(v: CalView) {
    this.view = v;
    this.#save();
    void this.refresh();
  }

  setWeekStart(w: WeekStart) {
    this.weekStart = w;
    this.#save();
    void this.refresh();
  }

  step(dir: 1 | -1) {
    this.anchor = shift(this.view, this.anchor, dir);
    void this.refresh();
  }

  today() {
    this.anchor = startOfDay(new Date());
    void this.refresh();
  }

  goToDay(d: Date) {
    this.anchor = startOfDay(d);
    this.view = "day";
    this.#save();
    void this.refresh();
  }

  openNew(start?: string, allDay = false) {
    this.dialog = { mode: "new", start, allDay };
  }

  async openOccurrence(occ: Occurrence) {
    try {
      const event = await api.eventGet(occ.event_id);
      this.dialog = { mode: "edit", event, occurrence: occ };
    } catch (e) {
      app.toast("error", errText(e));
    }
  }

  closeDialog() {
    this.dialog = null;
  }

  /** Create or change an event. Returns an error message for the dialog to show, or null on success. */
  async saveEvent(input: EventInput, id?: string, reminders?: number[]): Promise<string | null> {
    try {
      const ev = id ? await api.eventUpdate(id, input) : await api.eventCreate(input);
      if (reminders) await api.eventSetReminders(ev.id, reminders);
      this.dialog = null;
      await this.refresh();
      app.toast("success", id ? `Saved “${ev.title}”` : `Added “${ev.title}”`);
      return null;
    } catch (e) {
      return errText(e);
    }
  }

  async deleteEvent(id: string, instance?: string): Promise<string | null> {
    try {
      await api.eventDelete(id, instance);
      this.dialog = null;
      await this.refresh();
      app.toast("info", instance ? "Removed that occurrence" : "Event deleted");
      return null;
    } catch (e) {
      return errText(e);
    }
  }

  // ---------------------------------------------------------------- calendars

  colorOf(id: string): string {
    return this.calendars.find((c) => c.id === id)?.color ?? "#7aa2f7";
  }

  writable = $derived(this.calendars.filter((c) => !c.read_only));

  async toggleVisible(c: Calendar) {
    try {
      await api.calendarUpdate(c.id, { visible: !c.visible });
      await this.loadCalendars();
      await this.refresh();
    } catch (e) {
      app.toast("error", errText(e));
    }
  }

  async saveCalendar(existing: Calendar | null, name: string, color: string): Promise<string | null> {
    try {
      if (existing) await api.calendarUpdate(existing.id, { name, color });
      else await api.calendarCreate(name, color);
      this.calendarDialog = null;
      await this.loadCalendars();
      await this.refresh();
      return null;
    } catch (e) {
      return errText(e);
    }
  }

  async deleteCalendar(c: Calendar): Promise<string | null> {
    try {
      await api.calendarDelete(c.id);
      this.calendarDialog = null;
      await this.loadCalendars();
      await this.refresh();
      app.toast("info", `Deleted the calendar “${c.name}”`);
      return null;
    } catch (e) {
      return errText(e);
    }
  }

  // ---------------------------------------------------------------- sync

  get hasSynced() {
    return this.calendars.some((c) => c.kind !== "local");
  }

  /** Sync on a timer while the app is open. Safe to call more than once. */
  startAutoSync() {
    if (this.#timer) return;
    this.#timer = setInterval(() => {
      if (this.hasSynced && !this.syncing) void this.sync(undefined, true);
    }, AUTO_SYNC_MS);
  }

  /** Pull, then push, one calendar or all of them. `quiet` skips the toast when nothing happened. */
  async sync(calendar?: string, quiet = false) {
    if (this.syncing) return;
    this.syncing = true;
    try {
      const reports = await api.calendarSync(calendar);
      await this.loadCalendars();
      await this.refresh();
      this.summarize(reports, quiet);
    } catch (e) {
      if (!quiet) app.toast("error", errText(e));
    } finally {
      this.syncing = false;
    }
  }

  summarize(reports: CalSyncReport[], quiet: boolean) {
    const sum = (f: (r: CalSyncReport) => number) => reports.reduce((n, r) => n + f(r), 0);
    const conflicts = sum((r) => r.conflicts);
    const errors = reports.flatMap((r) => r.errors.map((e) => `${r.calendar}: ${e}`));
    const changed = sum((r) => r.pulled + r.removed + r.pushed);
    if (conflicts > 0) {
      app.toast("info", `Synced. ${conflicts} change${conflicts === 1 ? "" : "s"} conflicted; the server's version was kept.`, { label: "View log", run: () => void this.openLog() });
    } else if (errors.length > 0) {
      app.toast("error", errors[0] + (errors.length > 1 ? ` (+${errors.length - 1} more)` : ""), { label: "View log", run: () => void this.openLog() });
    } else if (!quiet) {
      app.toast("success", changed === 0 ? "Calendars are up to date" : `Synced: ${sum((r) => r.pulled + r.removed)} in, ${sum((r) => r.pushed)} out`);
    }
  }

  async openConnect() {
    this.connectOpen = true;
    await this.loadRemote();
  }

  async loadRemote() {
    this.remoteLoading = true;
    try {
      this.remote = await api.calendarRemote();
    } catch (e) {
      app.toast("error", errText(e));
    } finally {
      this.remoteLoading = false;
    }
  }

  /** Mirror a server calendar here, then fill it. */
  async link(source: string, accountId: string, remoteId: string): Promise<string | null> {
    try {
      const c = await api.calendarLink(source, accountId, remoteId);
      await this.loadCalendars();
      await this.loadRemote();
      void this.sync(c.id);
      return null;
    } catch (e) {
      return errText(e);
    }
  }

  async unlinkCalendar(c: Calendar): Promise<string | null> {
    try {
      await api.calendarUnlink(c.id);
      this.calendarDialog = null;
      await this.loadCalendars();
      await this.refresh();
      app.toast("info", `Stopped syncing “${c.name}”. Nothing was deleted on the server.`);
      return null;
    } catch (e) {
      return errText(e);
    }
  }

  async openLog() {
    this.logOpen = true;
    await this.loadLog();
  }

  async loadLog() {
    try {
      this.log = await api.syncLog(200);
    } catch (e) {
      app.toast("error", errText(e));
    }
  }

  async clearLog() {
    try {
      await api.syncLogClear();
      this.log = [];
    } catch (e) {
      app.toast("error", errText(e));
    }
  }

  async exportCalendar(c: Calendar) {
    try {
      const text = await api.calendarExport(c.id);
      const path = await shell.writeIcs(c.name, text);
      app.toast("success", `Saved ${path}`, { label: "Show", run: () => void shell.reveal(path) });
    } catch (e) {
      app.toast("error", errText(e));
    }
  }

  async importInto(c: Calendar) {
    try {
      const picked = await pickFiles({ multiple: false, filters: [{ name: "Calendar", extensions: ["ics"] }] });
      if (!picked || Array.isArray(picked)) return;
      const text = await shell.readIcs(picked);
      const r = await api.calendarImport(c.id, text);
      this.calendarDialog = null;
      await this.refresh();
      const skipped = r.skipped.length ? `, skipped ${r.skipped.length}` : "";
      app.toast(r.skipped.length ? "info" : "success", `Imported ${r.added} new, updated ${r.updated}${skipped}`);
    } catch (e) {
      app.toast("error", errText(e));
    }
  }
}

export const cal = new CalendarState();
export { addDays, browserTz };
