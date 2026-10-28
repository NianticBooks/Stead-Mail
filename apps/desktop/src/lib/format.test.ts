import { describe, expect, it } from "vitest";
import { formatBytes, formatEventTime, formatFullDate, formatListDate, isPastEvent, formatSize, initials, isStarred, isUnread, recipientLabel, senderLabel } from "./format";

const opts = { locale: "en-US", timeZone: "UTC", now: new Date("2026-10-28T15:00:00Z") };

describe("formatListDate", () => {
  it("shows the time for today", () => {
    expect(formatListDate("2026-10-28T09:05:00Z", opts)).toBe("9:05 AM");
  });
  it("shows the weekday within the last six days", () => {
    expect(formatListDate("2026-10-26T09:05:00Z", opts)).toBe("Mon");
  });
  it("shows month and day earlier in the year, and the year for older mail", () => {
    expect(formatListDate("2026-03-02T09:05:00Z", opts)).toBe("Mar 2");
    expect(formatListDate("2025-12-31T09:05:00Z", opts)).toBe("Dec 31, 2025");
  });
  it("is empty for missing or invalid input and never throws", () => {
    expect(formatListDate(null, opts)).toBe("");
    expect(formatListDate("not a date", opts)).toBe("");
  });
  it("treats a future date as a plain date, not a weekday", () => {
    expect(formatListDate("2026-11-02T09:05:00Z", opts)).toBe("Nov 2");
  });
  it("formats a full date", () => {
    expect(formatFullDate("2026-10-28T09:05:00Z", opts)).toContain("Oct 28, 2026");
    expect(formatFullDate("garbage", opts)).toBe("");
  });
});

describe("formatSize", () => {
  it("scales units", () => {
    expect(formatSize(0)).toBe("0 B");
    expect(formatSize(1023)).toBe("1023 B");
    expect(formatSize(251916)).toBe("246 KB");
    expect(formatSize(1536)).toBe("1.5 KB");
    expect(formatSize(5 * 1024 * 1024)).toBe("5.0 MB");
    expect(formatSize(-1)).toBe("");
  });
});

describe("labels", () => {
  it("prefers the display name, then the address", () => {
    expect(senderLabel({ from_name: "Jill Book", from_addr: "j@x.com" })).toBe("Jill Book");
    expect(senderLabel({ from_name: "  ", from_addr: "j@x.com" })).toBe("j@x.com");
    expect(senderLabel({ from_name: null, from_addr: null })).toBe("(unknown sender)");
  });
  it("makes initials", () => {
    expect(initials("Jill Book")).toBe("JB");
    expect(initials("fred@example.com")).toBe("FE");
    expect(initials("Mint")).toBe("M");
    expect(initials("")).toBe("?");
  });
  it("reads flags as whole tokens", () => {
    expect(isUnread({ flags: "" })).toBe(true);
    expect(isUnread({ flags: "\\Flagged \\Seen" })).toBe(false);
    expect(isStarred({ flags: "\\Flagged" })).toBe(true);
    expect(isStarred({ flags: "\\Seen" })).toBe(false);
    expect(isUnread({ flags: "\\Unseen-ish" })).toBe(true);
  });
});

describe("recipientLabel", () => {
  it("prefers the display name and counts the rest", () => {
    expect(recipientLabel("Jill Book <jill@example.com>")).toBe("Jill Book");
    expect(recipientLabel("jill@example.com")).toBe("jill@example.com");
    expect(recipientLabel("Jill Book <jill@example.com>, kobe@example.com, x@y.z")).toBe("Jill Book +2");
    expect(recipientLabel('"Book, Jill" <jill@example.com>, kobe@example.com')).toBe("Book, Jill +1");
    expect(recipientLabel("<only@example.com>")).toBe("only@example.com");
  });
  it("copes with empty input", () => {
    expect(recipientLabel(null)).toBe("(no recipient)");
    expect(recipientLabel("  ")).toBe("(no recipient)");
  });
});

describe("formatEventTime", () => {
  const o = { locale: "en-US", timeZone: "America/Chicago" };
  it("shows a timed event with its end", () => {
    expect(formatEventTime({ start_utc: "2026-10-29T19:30:00Z", end_utc: "2026-10-29T20:30:00Z", all_day: false }, o)).toBe("Thu, Oct 29 · 2:30 PM – 3:30 PM");
  });
  it("shows a start-only event", () => {
    expect(formatEventTime({ start_utc: "2026-10-29T19:30:00Z", end_utc: null, all_day: false }, o)).toBe("Thu, Oct 29 · 2:30 PM");
  });
  it("shows an all-day event without a time", () => {
    expect(formatEventTime({ start_utc: "2026-10-31T05:00:00Z", end_utc: "2026-11-01T05:00:00Z", all_day: true }, o)).toBe("Sat, Oct 31 · all day");
  });
  it("shows both days for an event that crosses midnight", () => {
    const t = formatEventTime({ start_utc: "2026-10-30T23:00:00Z", end_utc: "2026-10-31T07:00:00Z", all_day: false }, o);
    expect(t).toContain("Fri, Oct 30");
    expect(t).toContain("Sat, Oct 31");
  });
  it("uses the viewer's zone, not the sender's", () => {
    expect(formatEventTime({ start_utc: "2026-10-29T19:30:00Z", end_utc: null, all_day: false }, { locale: "en-US", timeZone: "America/New_York" })).toBe("Thu, Oct 29 · 3:30 PM");
  });
  it("never throws on junk", () => {
    expect(formatEventTime({ start_utc: "nonsense", end_utc: null, all_day: false })).toBe("");
  });
});

describe("isPastEvent", () => {
  const now = new Date("2026-10-28T12:00:00Z");
  it("compares the end (or start) with now", () => {
    expect(isPastEvent({ start_utc: "2026-10-27T12:00:00Z", end_utc: null, all_day: false }, now)).toBe(true);
    expect(isPastEvent({ start_utc: "2026-10-29T12:00:00Z", end_utc: null, all_day: false }, now)).toBe(false);
    expect(isPastEvent({ start_utc: "2026-10-28T10:00:00Z", end_utc: "2026-10-28T13:00:00Z", all_day: false }, now)).toBe(false);
  });
});

describe("formatBytes", () => {
  it("scales", () => {
    expect([formatBytes(12), formatBytes(2048), formatBytes(5 * 1024 * 1024)]).toEqual(["12 B", "2.0 KB", "5.0 MB"]);
  });
});
