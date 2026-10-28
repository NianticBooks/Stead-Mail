import { formatFullDate, senderLabel } from "./format";
import type { MessageDetail } from "./types";

export interface ComposeInit {
  to: string[];
  subject: string;
  body: string;
  inReplyTo?: string;
  references?: string[];
}

export function replySubject(subject: string | null): string {
  const s = (subject ?? "").trim();
  return /^re:\s*/i.test(s) ? s : `Re: ${s}`.trim();
}

const MAX_QUOTED_LINES = 200;

/** "On <date>, <sender> wrote:" followed by the original text, every line prefixed with "> ". */
export function quote(d: Pick<MessageDetail, "from_name" | "from_addr" | "date" | "internal_date" | "body_text">): string {
  const lines = (d.body_text ?? "").replace(/\r\n/g, "\n").trimEnd().split("\n");
  const kept = lines.slice(0, MAX_QUOTED_LINES).map((l) => (l.startsWith(">") ? `>${l}` : `> ${l}`.trimEnd()));
  if (lines.length > MAX_QUOTED_LINES) kept.push("> […]");
  const when = formatFullDate(d.date ?? d.internal_date);
  return `On ${when ? `${when}, ` : ""}${senderLabel(d)} wrote:\n${kept.join("\n")}`;
}

/** Prefill for replying to the sender of `d`. Threading headers keep the conversation intact. */
export function buildReply(d: MessageDetail): ComposeInit {
  const refs = (d.references ?? "").split(/\s+/).filter(Boolean);
  if (d.message_id) refs.push(d.message_id);
  return {
    to: d.from_addr ? [d.from_addr] : [],
    subject: replySubject(d.subject),
    body: `\n\n${quote(d)}\n`,
    inReplyTo: d.message_id ?? undefined,
    references: refs,
  };
}

/** Split a typed recipient list on commas, semicolons and whitespace; drop empties and repeats. */
export function parseRecipients(text: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const part of text.split(/[,;\s]+/)) {
    const a = part.trim().replace(/^<|>$/g, "");
    if (a && !seen.has(a.toLowerCase())) {
      seen.add(a.toLowerCase());
      out.push(a);
    }
  }
  return out;
}
