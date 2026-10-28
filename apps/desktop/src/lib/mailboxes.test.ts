import { describe, expect, it } from "vitest";
import { folderEntries, isQuietFolder } from "./mailboxes";
import type { Mailbox } from "./types";

const mb = (name: string, attributes = "", delimiter: string | null = "/"): Mailbox => ({
  id: Math.floor(Math.random() * 1e6), account_id: "a", name, delimiter, attributes,
  uidvalidity: null, uidnext: null, exists_count: null, last_synced_at: null, unseen: null,
});

describe("folderEntries", () => {
  it("orders the well-known folders first and hides unselectable containers", () => {
    const list = [
      mb("Orders", 'Extension("\\\\HasNoChildren")'),
      mb("[Gmail]", 'Extension("\\\\HasChildren") NoSelect'),
      mb("[Gmail]/Sent Mail", "Sent"),
      mb("Archive"),
      mb("[Gmail]/Drafts", "Drafts"),
      mb("INBOX", "NoInferiors"),
      mb("CENOIS"),
    ];
    const names = folderEntries(list).map((e) => e.mailbox.name);
    expect(names).toEqual(["INBOX", "[Gmail]/Drafts", "[Gmail]/Sent Mail", "Archive", "CENOIS", "Orders"]);
  });

  it("labels nested folders by their last segment and records depth", () => {
    const e = folderEntries([mb("INBOX/Long term save"), mb("[Gmail]/Sent Mail", "Sent")]);
    const nested = e.find((x) => x.mailbox.name === "INBOX/Long term save")!;
    expect(nested.label).toBe("Long term save");
    expect(nested.depth).toBe(1);
    expect(e.find((x) => x.mailbox.name.startsWith("[Gmail]"))!.label).toBe("Sent Mail");
  });

  it("recognises iCloud's Sent Messages and Drafts without special-use flags", () => {
    const e = folderEntries([mb("Sent Messages", "Sent"), mb("Sent Items"), mb("Drafts"), mb("Notes")]);
    expect(e.map((x) => x.icon)).toEqual(["file", "send", "send", "folder"]);
  });

  it("copes with servers that use a dot delimiter", () => {
    const e = folderEntries([mb("INBOX.Receipts", "", ".")]);
    expect(e[0].label).toBe("Receipts");
    expect(e[0].depth).toBe(1);
  });
});

describe("isQuietFolder", () => {
  it("flags trash, junk and all-mail", () => {
    expect(isQuietFolder(mb("[Gmail]/Trash", "Trash"))).toBe(true);
    expect(isQuietFolder(mb("[Gmail]/All Mail", "All"))).toBe(true);
    expect(isQuietFolder(mb("Junk"))).toBe(true);
    expect(isQuietFolder(mb("Deleted Messages", "Trash"))).toBe(true);
    expect(isQuietFolder(mb("Cabinet"))).toBe(false);
    expect(isQuietFolder(mb("INBOX"))).toBe(false);
  });
});
