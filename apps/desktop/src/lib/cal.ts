// Pure calendar helpers: date arithmetic, view ranges, and day layout. No DOM, no Svelte.
// Day boundaries use the browser's own zone (JS Date), which handles Daylight Saving for us;
// all-day events are the exception and are placed by the dates in their own zone.

import type { Occurrence } from "./types";

export type CalView = "month" | "week" | "day" | "agenda";
export type WeekStart = 0 | 1;

export const AGENDA_DAYS = 30;

export function startOfDay(d: Date): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate());
}

/** Add whole calendar days (not 24-hour blocks, so it is right across Daylight Saving). */
export function addDays(d: Date, n: number): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate() + n, d.getHours(), d.getMinutes());
}

export function startOfWeek(d: Date, weekStart: WeekStart): Date {
  const day = startOfDay(d);
  return addDays(day, -((day.getDay() - weekStart + 7) % 7));
}

export function sameDay(a: Date, b: Date): boolean {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
}

const pad = (n: number) => String(n).padStart(2, "0");

/** "YYYY-MM-DD" for a local date. */
export function dayKey(d: Date): string {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

export function parseDayKey(k: string): Date {
  const [y, m, d] = k.split("-").map(Number);
  return new Date(y, m - 1, d);
}

/** The window a view needs, as [from, to) in local midnights. Month shows six full weeks. */
export function rangeFor(view: CalView, anchor: Date, weekStart: WeekStart): { from: Date; to: Date } {
  const a = startOfDay(anchor);
  switch (view) {
    case "month": {
      const from = startOfWeek(new Date(a.getFullYear(), a.getMonth(), 1), weekStart);
      return { from, to: addDays(from, 42) };
    }
    case "week": {
      const from = startOfWeek(a, weekStart);
      return { from, to: addDays(from, 7) };
    }
    case "day":
      return { from: a, to: addDays(a, 1) };
    case "agenda":
      return { from: a, to: addDays(a, AGENDA_DAYS) };
  }
}

/** Move the anchor one step. Month steps land on the 1st so short months never skip. */
export function shift(view: CalView, anchor: Date, dir: 1 | -1): Date {
  const a = startOfDay(anchor);
  switch (view) {
    case "month":
      return new Date(a.getFullYear(), a.getMonth() + dir, 1);
    case "week":
      return addDays(a, 7 * dir);
    case "day":
      return addDays(a, dir);
    case "agenda":
      return addDays(a, AGENDA_DAYS * dir);
  }
}

export function daysIn(from: Date, to: Date): Date[] {
  const out: Date[] = [];
  for (let d = startOfDay(from); d < to; d = addDays(d, 1)) out.push(d);
  return out;
}

export function titleFor(view: CalView, anchor: Date, weekStart: WeekStart, locale?: string): string {
  const fmt = (o: Intl.DateTimeFormatOptions, d: Date) => new Intl.DateTimeFormat(locale, o).format(d);
  switch (view) {
    case "month":
      return fmt({ month: "long", year: "numeric" }, anchor);
    case "day":
      return fmt({ weekday: "long", month: "long", day: "numeric", year: "numeric" }, anchor);
    case "week": {
      const { from, to } = rangeFor("week", anchor, weekStart);
      const last = addDays(to, -1);
      const sameMonth = from.getMonth() === last.getMonth();
      const left = fmt({ month: "short", day: "numeric" }, from);
      const right = sameMonth ? String(last.getDate()) : fmt({ month: "short", day: "numeric" }, last);
      return `${left} – ${right}, ${last.getFullYear()}`;
    }
    case "agenda": {
      const { from, to } = rangeFor("agenda", anchor, weekStart);
      return `${fmt({ month: "short", day: "numeric" }, from)} – ${fmt({ month: "short", day: "numeric" }, addDays(to, -1))}`;
    }
  }
}

// ---------------------------------------------------------------- time zones

/** Wall-clock parts of an instant in a given IANA zone. */
export function zonedParts(iso: string, tz: string): { y: number; mo: number; d: number; h: number; mi: number } {
  const parts = new Intl.DateTimeFormat("en-US", {
    timeZone: tz, hourCycle: "h23", year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit",
  }).formatToParts(new Date(iso));
  const get = (t: string) => Number(parts.find((p) => p.type === t)?.value ?? 0);
  return { y: get("year"), mo: get("month"), d: get("day"), h: get("hour") % 24, mi: get("minute") };
}

export const browserTz = (): string => Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";

/** "YYYY-MM-DDTHH:MM" for an instant as seen in `tz` (what a datetime-local input wants). */
export function toLocalInput(iso: string, tz: string): string {
  const p = zonedParts(iso, tz);
  return `${p.y}-${pad(p.mo)}-${pad(p.d)}T${pad(p.h)}:${pad(p.mi)}`;
}

/** "YYYY-MM-DD" for an instant as seen in `tz`. */
export function toDateInput(iso: string, tz: string): string {
  return toLocalInput(iso, tz).slice(0, 10);
}

/** The instant a local wall-clock time ("YYYY-MM-DDTHH:MM") refers to in `tz`, or null if malformed.
 *  A time that falls in a Daylight Saving gap lands an hour later; one that repeats takes the first. */
export function zonedToUtc(local: string, tz: string): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/.exec(local);
  if (!m) return null;
  const guess = Date.UTC(+m[1], +m[2] - 1, +m[3], +m[4], +m[5]);
  const offset = (utcMs: number) => {
    const p = zonedParts(new Date(utcMs).toISOString(), tz);
    return Date.UTC(p.y, p.mo - 1, p.d, p.h, p.mi) - Math.floor(utcMs / 60000) * 60000;
  };
  // The zone's offset just before and just after `guess`: a transition can only change it in between.
  const DAY = 86_400_000;
  const candidates = [...new Set([guess - offset(guess - DAY), guess - offset(guess + DAY)])].filter(
    (t) => toLocalInput(new Date(t).toISOString(), tz) === local.slice(0, 16),
  );
  // A wall time that does not exist (spring-forward gap) is read with the offset from before the jump.
  const t = candidates.length ? Math.min(...candidates) : guess - offset(guess - DAY);
  return Number.isNaN(t) ? null : new Date(t);
}

// ---------------------------------------------------------------- placing occurrences on days

export interface Segment {
  occ: Occurrence;
  day: string;
  /** Minutes from local midnight; for all-day segments 0 and 1440. */
  startMin: number;
  endMin: number;
  /** All-day events and anything covering the whole day go in the banner row. */
  banner: boolean;
  continuesBefore: boolean;
  continuesAfter: boolean;
}

const minutesOf = (d: Date) => d.getHours() * 60 + d.getMinutes();

/** The local dates an all-day occurrence covers, taken in the event's own zone. */
export function allDayKeys(occ: Occurrence, fallbackTz: string): string[] {
  const tz = occ.tz || fallbackTz;
  const first = toDateInput(occ.start_utc, tz);
  const endEx = toDateInput(occ.end_utc, tz);
  const out: string[] = [];
  for (let d = parseDayKey(first), stop = parseDayKey(endEx); d < stop && out.length < 400; d = addDays(d, 1)) out.push(dayKey(d));
  return out.length ? out : [first];
}

/** Split occurrences into per-day segments for the given days. */
export function segmentsByDay(occs: Occurrence[], days: Date[], fallbackTz = browserTz()): Map<string, Segment[]> {
  const out = new Map<string, Segment[]>();
  for (const d of days) out.set(dayKey(d), []);
  for (const occ of occs) {
    if (occ.all_day) {
      const keys = allDayKeys(occ, fallbackTz);
      keys.forEach((k, i) => {
        out.get(k)?.push({ occ, day: k, startMin: 0, endMin: 1440, banner: true, continuesBefore: i > 0, continuesAfter: i < keys.length - 1 });
      });
      continue;
    }
    const s = new Date(occ.start_utc);
    const e = new Date(occ.end_utc);
    for (const day of days) {
      const ds = startOfDay(day);
      const de = addDays(ds, 1);
      if (s >= de || e <= ds) continue;
      const clipStart = s > ds ? s : ds;
      const clipEnd = e < de ? e : de;
      const startMin = clipStart.getTime() === ds.getTime() ? 0 : minutesOf(clipStart);
      const endMin = clipEnd.getTime() === de.getTime() ? 1440 : minutesOf(clipEnd);
      out.get(dayKey(day))?.push({
        occ, day: dayKey(day), startMin, endMin: Math.max(endMin, startMin + 1),
        banner: startMin === 0 && endMin === 1440, continuesBefore: s < ds, continuesAfter: e > de,
      });
    }
  }
  for (const list of out.values()) list.sort((a, b) => Number(b.banner) - Number(a.banner) || a.startMin - b.startMin || a.endMin - b.endMin || a.occ.title.localeCompare(b.occ.title));
  return out;
}

export interface Placed {
  seg: Segment;
  col: number;
  cols: number;
}

/** Side-by-side columns for overlapping timed segments (the usual week-view layout). */
export function layoutColumns(segs: Segment[], minLen = 20): Placed[] {
  const timed = segs.filter((s) => !s.banner).map((seg) => ({ seg, end: Math.max(seg.endMin, seg.startMin + minLen) }));
  timed.sort((a, b) => a.seg.startMin - b.seg.startMin || b.end - a.end);
  const placed: Placed[] = [];
  let group: { item: (typeof timed)[number]; col: number }[] = [];
  let groupEnd = -1;
  const flush = () => {
    const cols = group.reduce((m, g) => Math.max(m, g.col + 1), 0);
    for (const g of group) placed.push({ seg: g.item.seg, col: g.col, cols });
    group = [];
  };
  for (const item of timed) {
    if (group.length && item.seg.startMin >= groupEnd) {
      flush();
      groupEnd = -1;
    }
    const used = new Set(group.filter((g) => g.item.end > item.seg.startMin).map((g) => g.col));
    let col = 0;
    while (used.has(col)) col++;
    group.push({ item, col });
    groupEnd = Math.max(groupEnd, item.end);
  }
  flush();
  return placed;
}

// ---------------------------------------------------------------- repeat presets

export type RepeatKind = "none" | "daily" | "weekdays" | "weekly" | "monthly" | "yearly" | "custom";

const DAY_CODES = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"];

/** The rule for a preset, relative to the event's first day. Custom keeps whatever text was typed. */
export function ruleFor(kind: RepeatKind, firstDay: Date, custom = ""): string | undefined {
  switch (kind) {
    case "none": return undefined;
    case "daily": return "FREQ=DAILY";
    case "weekdays": return "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR";
    case "weekly": return `FREQ=WEEKLY;BYDAY=${DAY_CODES[firstDay.getDay()]}`;
    case "monthly": return `FREQ=MONTHLY;BYMONTHDAY=${firstDay.getDate()}`;
    case "yearly": return "FREQ=YEARLY";
    case "custom": return custom.trim() || undefined;
  }
}

/** Which preset an existing rule corresponds to (custom when none fits exactly). */
export function kindOf(rule: string | null, firstDay: Date): RepeatKind {
  if (!rule) return "none";
  for (const k of ["daily", "weekdays", "weekly", "monthly", "yearly"] as const) if (ruleFor(k, firstDay) === rule.toUpperCase()) return k;
  return "custom";
}

export function describeRule(rule: string | null): string {
  if (!rule) return "";
  const r = rule.toUpperCase();
  const get = (k: string) => new RegExp(`(?:^|;)${k}=([^;]+)`).exec(r)?.[1];
  const freq = get("FREQ");
  const n = Number(get("INTERVAL") ?? 1);
  const unit = { DAILY: "day", WEEKLY: "week", MONTHLY: "month", YEARLY: "year" }[freq ?? ""];
  if (!unit) return "Repeats";
  let s = n > 1 ? `Every ${n} ${unit}s` : `Every ${unit}`;
  if (get("BYDAY") === "MO,TU,WE,TH,FR") s = "Every weekday";
  const count = get("COUNT");
  if (count) s += `, ${count} times`;
  else if (get("UNTIL")) s += ", until a set date";
  return s;
}

// ---------------------------------------------------------------- display

export function formatClock(iso: string, tz?: string | null, locale?: string): string {
  return new Intl.DateTimeFormat(locale, { hour: "numeric", minute: "2-digit", ...(tz ? { timeZone: tz } : {}) }).format(new Date(iso));
}

export function formatHour(h: number, locale?: string): string {
  return new Intl.DateTimeFormat(locale, { hour: "numeric" }).format(new Date(2000, 0, 1, h));
}

/** "Thu, Oct 29 · 9:00 AM – 9:30 AM", or the date range for all-day events. */
export function formatWhen(occ: Pick<Occurrence, "start_utc" | "end_utc" | "all_day" | "tz">, locale?: string): string {
  const tz = occ.tz || browserTz();
  const day = (iso: string) => new Intl.DateTimeFormat(locale, { weekday: "short", month: "short", day: "numeric", timeZone: tz }).format(new Date(iso));
  if (occ.all_day) {
    const keys = allDayKeys({ ...occ } as Occurrence, tz);
    const last = keys[keys.length - 1];
    const fmt = (k: string) => new Intl.DateTimeFormat(locale, { weekday: "short", month: "short", day: "numeric" }).format(parseDayKey(k));
    return keys.length > 1 ? `${fmt(keys[0])} – ${fmt(last)}` : `${fmt(keys[0])} · all day`;
  }
  const sameDayInTz = toDateInput(occ.start_utc, tz) === toDateInput(occ.end_utc, tz);
  return sameDayInTz
    ? `${day(occ.start_utc)} · ${formatClock(occ.start_utc, tz, locale)} – ${formatClock(occ.end_utc, tz, locale)}`
    : `${day(occ.start_utc)} ${formatClock(occ.start_utc, tz, locale)} – ${day(occ.end_utc)} ${formatClock(occ.end_utc, tz, locale)}`;
}

export const CALENDAR_COLORS = ["#7aa2f7", "#9ece6a", "#e0af68", "#f7768e", "#bb9af7", "#7dcfff", "#ff9e64", "#73daca"];
