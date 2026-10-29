import { describe, expect, it } from "vitest";
import {
  addDays, allDayKeys, dayKey, daysIn, describeRule, kindOf, layoutColumns, rangeFor, ruleFor, segmentsByDay, shift, startOfWeek, toLocalInput, zonedParts, zonedToUtc,
} from "./cal";
import type { Occurrence } from "./types";

const d = (y: number, m: number, day: number, h = 0, mi = 0) => new Date(y, m - 1, day, h, mi);

function occ(over: Partial<Occurrence>): Occurrence {
  return {
    event_id: "e", calendar_id: "c", title: "T", location: null, start_utc: "2026-10-29T14:00:00Z", end_utc: "2026-10-29T15:00:00Z",
    all_day: false, tz: "America/Chicago", recurring: false, status: "confirmed", color: "#fff", instance: "x", ...over,
  };
}

describe("date arithmetic", () => {
  it("steps whole days across daylight saving", () => {
    expect(dayKey(addDays(d(2026, 10, 31), 2))).toBe("2026-11-02");
    expect(dayKey(addDays(d(2026, 3, 7), 2))).toBe("2026-03-09");
    expect(dayKey(addDays(d(2026, 1, 31), 1))).toBe("2026-02-01");
  });

  it("finds the start of the week for either convention", () => {
    expect(dayKey(startOfWeek(d(2026, 10, 29), 0))).toBe("2026-10-25"); // Thursday -> Sunday
    expect(dayKey(startOfWeek(d(2026, 10, 29), 1))).toBe("2026-10-26"); // -> Monday
    expect(dayKey(startOfWeek(d(2026, 10, 25), 0))).toBe("2026-10-25");
    expect(dayKey(startOfWeek(d(2026, 10, 25), 1))).toBe("2026-10-19");
  });
});

describe("view ranges", () => {
  it("month always shows six whole weeks starting on the week start", () => {
    const r = rangeFor("month", d(2026, 10, 29), 0);
    expect(dayKey(r.from)).toBe("2026-09-27");
    expect(daysIn(r.from, r.to)).toHaveLength(42);
    expect(dayKey(rangeFor("month", d(2026, 2, 10), 1).from)).toBe("2026-01-26");
  });

  it("week and day cover exactly what they say", () => {
    const w = rangeFor("week", d(2026, 11, 1), 0); // the DST-change day
    expect(daysIn(w.from, w.to).map(dayKey)).toEqual(["2026-11-01", "2026-11-02", "2026-11-03", "2026-11-04", "2026-11-05", "2026-11-06", "2026-11-07"]);
    expect(daysIn(...(Object.values(rangeFor("day", d(2026, 11, 1), 0)) as [Date, Date]))).toHaveLength(1);
  });

  it("moving by month lands on the 1st so short months do not skip", () => {
    expect(dayKey(shift("month", d(2026, 1, 31), 1))).toBe("2026-02-01");
    expect(dayKey(shift("month", d(2026, 3, 31), -1))).toBe("2026-02-01");
    expect(dayKey(shift("month", d(2026, 12, 15), 1))).toBe("2027-01-01");
    expect(dayKey(shift("week", d(2026, 10, 29), -1))).toBe("2026-10-22");
    expect(dayKey(shift("day", d(2026, 10, 31), 1))).toBe("2026-11-01");
  });
});

describe("time zones", () => {
  it("reads the wall clock of an instant in a zone", () => {
    expect(zonedParts("2026-10-29T14:00:00Z", "America/Chicago")).toEqual({ y: 2026, mo: 10, d: 29, h: 9, mi: 0 });
    expect(zonedParts("2026-11-05T15:00:00Z", "America/Chicago")).toEqual({ y: 2026, mo: 11, d: 5, h: 9, mi: 0 });
    expect(zonedParts("2026-10-29T04:30:00Z", "Asia/Kolkata")).toEqual({ y: 2026, mo: 10, d: 29, h: 10, mi: 0 });
    expect(zonedParts("2026-10-29T00:00:00Z", "UTC").h).toBe(0);
  });

  it("formats a datetime-local value", () => {
    expect(toLocalInput("2026-10-29T19:30:00Z", "America/Chicago")).toBe("2026-10-29T14:30");
    expect(toLocalInput("2026-10-29T05:05:00Z", "UTC")).toBe("2026-10-29T05:05");
  });
});

describe("zonedToUtc", () => {
  const iso = (l: string, tz: string) => zonedToUtc(l, tz)?.toISOString();
  it("converts ordinary local times", () => {
    expect(iso("2026-10-29T09:00", "America/Chicago")).toBe("2026-10-29T14:00:00.000Z");
    expect(iso("2026-11-05T09:00", "America/Chicago")).toBe("2026-11-05T15:00:00.000Z");
    expect(iso("2026-10-29T09:00", "Asia/Kolkata")).toBe("2026-10-29T03:30:00.000Z");
    expect(iso("2026-10-29T09:00", "UTC")).toBe("2026-10-29T09:00:00.000Z");
  });
  it("handles the spring-forward gap and the fall-back repeat", () => {
    expect(iso("2026-03-08T02:30", "America/Chicago")).toBe("2026-03-08T08:30:00.000Z"); // 03:30 CDT
    expect(iso("2026-11-01T01:30", "America/Chicago")).toBe("2026-11-01T06:30:00.000Z"); // first 1:30, CDT
  });
  it("round-trips with toLocalInput and rejects junk", () => {
    for (const l of ["2026-01-15T00:00", "2026-07-04T23:59", "2026-12-31T12:30"]) expect(toLocalInput(zonedToUtc(l, "Europe/London")!.toISOString(), "Europe/London")).toBe(l);
    expect(zonedToUtc("tomorrow", "UTC")).toBeNull();
  });
});

describe("all-day events", () => {
  it("cover the dates of their own zone, not the browser's", () => {
    const trip = occ({ all_day: true, start_utc: "2026-10-31T05:00:00Z", end_utc: "2026-11-03T06:00:00Z" });
    expect(allDayKeys(trip, "UTC")).toEqual(["2026-10-31", "2026-11-01", "2026-11-02"]);
    expect(allDayKeys(occ({ all_day: true, start_utc: "2026-10-31T05:00:00Z", end_utc: "2026-11-01T05:00:00Z" }), "Asia/Tokyo")).toEqual(["2026-10-31"]);
  });
});

describe("segmentsByDay", () => {
  const days = daysIn(d(2026, 10, 28), d(2026, 11, 3));

  it("puts a timed event on its day with minutes from local midnight", () => {
    // 14:00Z is 9:00 AM Chicago; the test machine's zone may differ, so build from local time.
    const start = d(2026, 10, 29, 9, 0);
    const end = d(2026, 10, 29, 10, 30);
    const m = segmentsByDay([occ({ start_utc: start.toISOString(), end_utc: end.toISOString() })], days);
    const segs = m.get("2026-10-29")!;
    expect(segs).toHaveLength(1);
    expect(segs[0]).toMatchObject({ startMin: 540, endMin: 630, banner: false, continuesBefore: false, continuesAfter: false });
    expect(m.get("2026-10-30")).toEqual([]);
  });

  it("splits an overnight event across both days", () => {
    const m = segmentsByDay([occ({ start_utc: d(2026, 10, 29, 22, 0).toISOString(), end_utc: d(2026, 10, 30, 2, 0).toISOString() })], days);
    expect(m.get("2026-10-29")![0]).toMatchObject({ startMin: 1320, endMin: 1440, continuesAfter: true, continuesBefore: false });
    expect(m.get("2026-10-30")![0]).toMatchObject({ startMin: 0, endMin: 120, continuesBefore: true, continuesAfter: false });
  });

  it("an event ending exactly at midnight does not leak into the next day", () => {
    const m = segmentsByDay([occ({ start_utc: d(2026, 10, 29, 23, 0).toISOString(), end_utc: d(2026, 10, 30, 0, 0).toISOString() })], days);
    expect(m.get("2026-10-29")).toHaveLength(1);
    expect(m.get("2026-10-30")).toEqual([]);
  });

  it("all-day events become banners on every covered day, listed first", () => {
    const banner = occ({ title: "Trip", all_day: true, tz: "America/Chicago", start_utc: "2026-10-31T05:00:00Z", end_utc: "2026-11-02T06:00:00Z" });
    const timed = occ({ title: "Lunch", start_utc: d(2026, 10, 31, 12, 0).toISOString(), end_utc: d(2026, 10, 31, 13, 0).toISOString() });
    const m = segmentsByDay([timed, banner], days, "America/Chicago");
    expect(m.get("2026-10-31")!.map((s) => s.occ.title)).toEqual(["Trip", "Lunch"]);
    expect(m.get("2026-11-01")!.map((s) => [s.occ.title, s.continuesBefore, s.continuesAfter])).toEqual([["Trip", true, false]]);
    expect(m.get("2026-10-30")).toEqual([]);
  });
});

describe("layoutColumns", () => {
  const seg = (title: string, startMin: number, endMin: number) => ({ occ: occ({ title }), day: "x", startMin, endMin, banner: false, continuesBefore: false, continuesAfter: false });

  it("keeps non-overlapping events full width", () => {
    const p = layoutColumns([seg("a", 60, 120), seg("b", 120, 180)]);
    expect(p.map((x) => [x.seg.occ.title, x.col, x.cols])).toEqual([["a", 0, 1], ["b", 0, 1]]);
  });

  it("puts overlapping events side by side and reuses freed columns", () => {
    const p = layoutColumns([seg("a", 60, 180), seg("b", 90, 120), seg("c", 130, 170), seg("d", 200, 230)]);
    const by = Object.fromEntries(p.map((x) => [x.seg.occ.title, [x.col, x.cols]]));
    expect(by).toEqual({ a: [0, 2], b: [1, 2], c: [1, 2], d: [0, 1] });
  });

  it("gives very short events a minimum height so they can overlap sensibly", () => {
    const p = layoutColumns([seg("a", 60, 61), seg("b", 70, 71)]);
    expect(p.map((x) => x.cols)).toEqual([2, 2]);
  });

  it("ignores banners", () => {
    expect(layoutColumns([{ ...seg("a", 0, 1440), banner: true }])).toEqual([]);
  });
});

describe("repeat presets", () => {
  const thu = d(2026, 10, 29);
  it("builds rules relative to the first day", () => {
    expect(ruleFor("weekly", thu)).toBe("FREQ=WEEKLY;BYDAY=TH");
    expect(ruleFor("monthly", thu)).toBe("FREQ=MONTHLY;BYMONTHDAY=29");
    expect(ruleFor("none", thu)).toBeUndefined();
    expect(ruleFor("custom", thu, "  FREQ=DAILY;COUNT=3 ")).toBe("FREQ=DAILY;COUNT=3");
    expect(ruleFor("custom", thu, " ")).toBeUndefined();
  });

  it("recognises presets and falls back to custom", () => {
    expect(kindOf(null, thu)).toBe("none");
    expect(kindOf("FREQ=WEEKLY;BYDAY=TH", thu)).toBe("weekly");
    expect(kindOf("FREQ=WEEKLY;BYDAY=FR", thu)).toBe("custom");
    expect(kindOf("freq=daily", thu)).toBe("daily");
  });

  it("describes rules in words", () => {
    expect(describeRule("FREQ=WEEKLY;BYDAY=TH")).toBe("Every week");
    expect(describeRule("FREQ=DAILY;INTERVAL=2")).toBe("Every 2 days");
    expect(describeRule("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR")).toBe("Every weekday");
    expect(describeRule("FREQ=MONTHLY;COUNT=6")).toBe("Every month, 6 times");
    expect(describeRule(null)).toBe("");
  });
});
