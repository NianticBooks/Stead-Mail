import type { Mailbox } from "./types";

export interface FolderEntry {
  mailbox: Mailbox;
  label: string;
  depth: number;
  icon: "inbox" | "file" | "send" | "archive" | "star" | "folder";
}

const lower = (s: string) => s.toLowerCase();
const attrs = (m: Mailbox) => lower(m.attributes).split(/\s+/);

function kind(m: Mailbox): FolderEntry["icon"] {
  const name = lower(m.name);
  const a = attrs(m);
  if (name === "inbox") return "inbox";
  if (a.includes("drafts") || /(^|\/)drafts?$/.test(name)) return "file";
  if (a.includes("sent") || /(^|\/)sent( items| messages| mail)?$/.test(name)) return "send";
  if (a.includes("archive") || /(^|\/)archive$/.test(name)) return "archive";
  if (a.includes("flagged")) return "star";
  return "folder";
}

const RANK: Record<FolderEntry["icon"], number> = { inbox: 0, file: 1, send: 2, archive: 3, star: 4, folder: 5 };

/** Selectable folders in display order: Inbox, Drafts, Sent, Archive, then the rest alphabetically. */
export function folderEntries(mailboxes: Mailbox[]): FolderEntry[] {
  return mailboxes
    .filter((m) => !attrs(m).includes("noselect"))
    .map((mailbox) => {
      const parts = mailbox.name.split(mailbox.delimiter || "/");
      return { mailbox, label: parts[parts.length - 1] || mailbox.name, depth: Math.max(0, parts.length - 1), icon: kind(mailbox) };
    })
    .sort((a, b) => RANK[a.icon] - RANK[b.icon] || a.mailbox.name.localeCompare(b.mailbox.name, undefined, { sensitivity: "base" }));
}

/** Folders that are noise by default: Trash, Junk, and the All Mail duplicate. */
export function isQuietFolder(m: Mailbox): boolean {
  const a = attrs(m);
  const last = lower(m.name).split("/").pop() ?? "";
  return a.includes("trash") || a.includes("junk") || a.includes("all") || /\b(junk|spam|trash|deleted)\b/.test(last);
}

/** The Trash folder: marked by the server, or named like one. */
export function isTrashFolder(m: Mailbox): boolean {
  const last = lower(m.name).split(m.delimiter || "/").pop() ?? "";
  return attrs(m).some((a) => a.includes("trash")) || ["trash", "bin", "deleted items", "deleted messages"].includes(last);
}
