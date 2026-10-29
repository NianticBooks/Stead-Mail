import { describe, expect, it } from "vitest";
import { buildReply, parseRecipients, quote, replySubject } from "./reply";
import type { MessageDetail } from "./types";

const detail = (over: Partial<MessageDetail> = {}): MessageDetail => ({
  id: 1, mailbox_id: 1, uid: 7, message_id: "<m2@x.com>", in_reply_to: "<m1@x.com>", references: "<root@x.com> <m1@x.com>",
  subject: "Dinner?", from_name: "Jill Book", from_addr: "jill@example.com", to_addrs: "fred@example.com",
  date: "2026-10-28T15:00:00Z", internal_date: null, size: 10, flags: "", thread_id: "<root@x.com>", preview: null,
  has_attachments: false, body_cached: true, triage: null, pending_events: 0, mailbox: "INBOX", body_text: "Six o'clock?\r\n\r\nBring wine.", body_html: null, attachments: [],
  ...over,
});

describe("replySubject", () => {
  it("adds Re: once", () => {
    expect(replySubject("Dinner?")).toBe("Re: Dinner?");
    expect(replySubject("Re: Dinner?")).toBe("Re: Dinner?");
    expect(replySubject("RE:Dinner?")).toBe("RE:Dinner?");
    expect(replySubject(null)).toBe("Re:");
  });
});

describe("quote", () => {
  it("prefixes every line and keeps blank lines readable", () => {
    const q = quote(detail());
    expect(q).toContain("Jill Book wrote:");
    expect(q.split("\n").slice(1)).toEqual(["> Six o'clock?", ">", "> Bring wine."]);
  });
  it("nests an already quoted line without a stray space", () => {
    expect(quote(detail({ body_text: "> earlier\nnow" })).split("\n").slice(1)).toEqual([">> earlier", "> now"]);
  });
  it("caps very long messages", () => {
    const q = quote(detail({ body_text: Array.from({ length: 500 }, (_, i) => `line ${i}`).join("\n") })).split("\n");
    expect(q.length).toBe(1 + 200 + 1);
    expect(q[q.length - 1]).toBe("> […]");
  });
  it("copes with no body", () => {
    expect(quote(detail({ body_text: null }))).toContain("wrote:");
  });
});

describe("buildReply", () => {
  it("addresses the sender and extends the References chain with the original", () => {
    const r = buildReply(detail());
    expect(r.to).toEqual(["jill@example.com"]);
    expect(r.subject).toBe("Re: Dinner?");
    expect(r.inReplyTo).toBe("<m2@x.com>");
    expect(r.references).toEqual(["<root@x.com>", "<m1@x.com>", "<m2@x.com>"]);
    expect(r.body.startsWith("\n\nOn ")).toBe(true);
  });
  it("still works when the original had no Message-ID or sender address", () => {
    const r = buildReply(detail({ message_id: null, from_addr: null, references: null }));
    expect(r.to).toEqual([]);
    expect(r.inReplyTo).toBeUndefined();
    expect(r.references).toEqual([]);
  });
});

describe("parseRecipients", () => {
  it("splits on commas, semicolons and spaces and removes duplicates", () => {
    expect(parseRecipients("a@x.com, b@x.com; <c@x.com>  A@X.com")).toEqual(["a@x.com", "b@x.com", "c@x.com"]);
    expect(parseRecipients("  ")).toEqual([]);
  });
});

describe("buildReplyAll", () => {
  it("fills To and Cc from the core and keeps threading and quoting", async () => {
    const { buildReplyAll } = await import("./reply");
    const r = buildReplyAll(detail(), { to: ["a@x.com"], cc: ["b@x.com", "c@x.com"] });
    expect(r.to).toEqual(["a@x.com"]);
    expect(r.cc).toEqual(["b@x.com", "c@x.com"]);
    expect(r.inReplyTo).toBe("<m2@x.com>");
    expect(r.subject).toMatch(/^Re: /);
  });
});
