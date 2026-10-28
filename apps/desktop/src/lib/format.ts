import type { MessageRow } from "./types";

const DAY = 24 * 60 * 60 * 1000;

export interface FormatOptions {
  now?: Date;
  locale?: string;
  timeZone?: string;
}

/** Compact date for list rows: time today, weekday this week, "Oct 27", then with the year. */
export function formatListDate(iso: string | null, opts: FormatOptions = {}): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const { now = new Date(), locale, timeZone } = opts;
  const day = (x: Date) => new Intl.DateTimeFormat("en-CA", { timeZone }).format(x); // yyyy-mm-dd
  if (day(d) === day(now)) return new Intl.DateTimeFormat(locale, { hour: "numeric", minute: "2-digit", timeZone }).format(d);
  const age = now.getTime() - d.getTime();
  if (age > 0 && age < 6 * DAY) return new Intl.DateTimeFormat(locale, { weekday: "short", timeZone }).format(d);
  const sameYear = new Intl.DateTimeFormat("en", { year: "numeric", timeZone }).format(d) === new Intl.DateTimeFormat("en", { year: "numeric", timeZone }).format(now);
  return new Intl.DateTimeFormat(locale, sameYear ? { month: "short", day: "numeric", timeZone } : { month: "short", day: "numeric", year: "numeric", timeZone }).format(d);
}

export function formatFullDate(iso: string | null, opts: FormatOptions = {}): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return new Intl.DateTimeFormat(opts.locale, { dateStyle: "medium", timeStyle: "short", timeZone: opts.timeZone }).format(d);
}

export function formatSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB"];
  let v = bytes / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}

export function senderLabel(m: Pick<MessageRow, "from_name" | "from_addr">): string {
  return m.from_name?.trim() || m.from_addr || "(unknown sender)";
}

export function initials(label: string): string {
  // An address has no words: use the first letters of the name part and the domain.
  const at = label.indexOf("@");
  if (at > 0 && !/\s/.test(label)) {
    return ((label[0] ?? "") + (label[at + 1] ?? "")).toUpperCase() || "?";
  }
  const words = label.replace(/[<>"@._-]+/g, " ").trim().split(/\s+/).filter(Boolean);
  if (words.length === 0) return "?";
  const first = words[0][0] ?? "?";
  const second = words.length > 1 ? (words[words.length - 1][0] ?? "") : "";
  return (first + second).toUpperCase();
}

export const isUnread = (m: Pick<MessageRow, "flags">): boolean => !m.flags.split(/\s+/).includes("\\Seen");
export const isStarred = (m: Pick<MessageRow, "flags">): boolean => m.flags.split(/\s+/).includes("\\Flagged");

/** First recipient of a To header value ("Name <a@b.c>, d@e.f") as a display label. */
export function recipientLabel(toAddrs: string | null): string {
  if (!toAddrs?.trim()) return "(no recipient)";
  // Split on commas that are not inside <...> or quotes.
  const parts: string[] = [];
  let cur = "";
  let depth = 0;
  let quoted = false;
  for (const ch of toAddrs) {
    if (ch === '"') quoted = !quoted;
    else if (!quoted && ch === "<") depth++;
    else if (!quoted && ch === ">") depth = Math.max(0, depth - 1);
    if (ch === "," && !quoted && depth === 0) {
      parts.push(cur);
      cur = "";
    } else cur += ch;
  }
  if (cur.trim()) parts.push(cur);
  const first = parts[0]?.trim() ?? "";
  const m = first.match(/^"?([^"<]*?)"?\s*<([^>]+)>$/);
  const label = (m ? m[1].trim() || m[2] : first).trim() || "(no recipient)";
  return parts.length > 1 ? `${label} +${parts.length - 1}` : label;
}

export interface EventTimeSource {
  start_utc: string;
  end_utc: string | null;
  all_day: boolean;
}

/** "Thu, Oct 29 · 2:30 – 3:30 PM", or "Sat, Oct 31 · all day", in the viewer's own time zone. */
export function formatEventTime(e: EventTimeSource, opts: FormatOptions = {}): string {
  const start = new Date(e.start_utc);
  if (Number.isNaN(start.getTime())) return "";
  const { locale, timeZone } = opts;
  const day = new Intl.DateTimeFormat(locale, { weekday: "short", month: "short", day: "numeric", timeZone }).format(start);
  if (e.all_day) {
    // An all-day event is anchored at local midnight; show the day only.
    return `${day} · all day`;
  }
  const time = (d: Date) => new Intl.DateTimeFormat(locale, { hour: "numeric", minute: "2-digit", timeZone }).format(d);
  const end = e.end_utc ? new Date(e.end_utc) : null;
  if (end && !Number.isNaN(end.getTime()) && end > start) {
    const sameDay = new Intl.DateTimeFormat("en-CA", { timeZone }).format(start) === new Intl.DateTimeFormat("en-CA", { timeZone }).format(end);
    if (sameDay) return `${day} · ${time(start)} – ${time(end)}`;
    return `${day} ${time(start)} – ${new Intl.DateTimeFormat(locale, { weekday: "short", month: "short", day: "numeric", timeZone }).format(end)} ${time(end)}`;
  }
  return `${day} · ${time(start)}`;
}

/** An event that has already started (or, if it has an end, finished) is in the past. */
export function isPastEvent(e: EventTimeSource, now: Date = new Date()): boolean {
  const end = e.end_utc ? new Date(e.end_utc) : new Date(e.start_utc);
  return !Number.isNaN(end.getTime()) && end.getTime() < now.getTime();
}

export const categoryLabel: Record<string, string> = {
  conversation: "Conversation",
  scheduling: "Scheduling",
  transaction: "Transaction",
  notification: "Notification",
  newsletter: "Newsletter",
  other: "Other",
};

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}
