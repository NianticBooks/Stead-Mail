import { listen } from "@tauri-apps/api/event";
import { open as pickFiles } from "@tauri-apps/plugin-dialog";
import { api, shell } from "./api";
import type { Draft } from "./types";
import { parseRecipients, buildReply, buildReplyAll, type ComposeInit } from "./reply";
import { isUnread } from "./format";
import { folderEntries } from "./mailboxes";
import type {
  Account, AddAccountInput, AiConfig, AiStatus, CloudPreview, EventProposal, MessageDetail, MessageRow, Mailbox, OutboxItem,
  ProviderKind, SanitizedHtml, SearchHit, SyncReport,
} from "./types";

const PAGE = 100;

export interface Toast {
  id: number;
  kind: "info" | "success" | "error";
  text: string;
  action?: { label: string; run: () => void };
}

export interface Compose {
  accountId: string;
  to: string;
  cc: string;
  bcc: string;
  showCc: boolean;
  subject: string;
  body: string;
  attachments: { path: string; name: string }[];
  inReplyTo?: string;
  references: string[];
  sending: boolean;
  error: string | null;
  dirty: boolean;
  /** Set once the message has been saved as a draft; later saves update the same one. */
  draftId?: string;
  /** The message being answered. AI drafting needs it; a brand new message has none. */
  replyTo?: { account: string; mailbox: string; uid: number };
  ai: { open: boolean; instruction: string; provider: ProviderKind; busy: boolean; error: string | null; preview: CloudPreview | null; undo: string | null };
}

/** Split a reply body into what the user has typed and the quoted original ("On ... wrote:" onward). */
export function splitReplyBody(body: string): { typed: string; quoted: string } {
  const m = /^On .{3,200} wrote:\s*$/m.exec(body);
  if (!m) return { typed: body.trim(), quoted: "" };
  return { typed: body.slice(0, m.index).trim(), quoted: body.slice(m.index) };
}

interface Prefs {
  markReadOnOpen: boolean;
}

const PREFS_KEY = "mailcore.prefs";

function loadPrefs(): Prefs {
  try {
    return { markReadOnOpen: true, ...JSON.parse(localStorage.getItem(PREFS_KEY) ?? "{}") };
  } catch {
    return { markReadOnOpen: true };
  }
}

const errText = (e: unknown) => (typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e));

class App {
  /** Which half of the app is showing. */
  view = $state<"mail" | "calendar">("mail");
  accounts = $state<Account[]>([]);
  accountId = $state<string | null>(null);
  mailboxes = $state<Mailbox[]>([]);
  mailbox = $state("INBOX");

  messages = $state<MessageRow[]>([]);
  selectedUid = $state<number | null>(null);
  detail = $state<MessageDetail | null>(null);
  html = $state<SanitizedHtml | null>(null);
  allowImages = $state(false);
  thread = $state<MessageRow[]>([]);

  loadingList = $state(false);
  loadingMessage = $state(false);
  messageError = $state<string | null>(null);
  syncing = $state(false);
  booted = $state(false);

  query = $state("");
  searchAll = $state(false);
  hits = $state<SearchHit[] | null>(null);
  searching = $state(false);

  aiConfig = $state<AiConfig | null>(null);
  aiStatus = $state<AiStatus | null>(null);
  analysing = $state(false);
  aiErrors = $state<string[]>([]);
  proposals = $state<EventProposal[]>([]);
  needsReplyOnly = $state(false);
  analysingFolder = $state(false);

  outbox = $state<OutboxItem[]>([]);
  toasts = $state<Toast[]>([]);
  compose = $state<Compose | null>(null);
  settingsOpen = $state(false);
  outboxOpen = $state(false);
  draftsOpen = $state(false);
  drafts = $state<Draft[]>([]);
  helpOpen = $state(false);
  prefs = $state<Prefs>(loadPrefs());

  #openToken = 0;
  #listToken = 0;
  #toastId = 0;
  #refreshTimer: ReturnType<typeof setTimeout> | null = null;

  account = $derived(this.accounts.find((a) => a.id === this.accountId) ?? null);
  currentMailbox = $derived(this.mailboxes.find((m) => m.name === this.mailbox) ?? null);
  folders = $derived(folderEntries(this.mailboxes));
  pendingOutbox = $derived(this.outbox.filter((o) => o.state === "queued" || o.state === "sending" || o.state === "failed").length);
  failedOutbox = $derived(this.outbox.filter((o) => o.state === "failed").length);
  aiEnabled = $derived(this.aiConfig?.enabled ?? false);
  rows = $derived.by<(MessageRow | SearchHit)[]>(() => {
    const base = this.hits ?? this.messages;
    return this.needsReplyOnly && this.hits === null ? base.filter((m) => m.triage?.needs_reply) : base;
  });
  needsReplyCount = $derived(this.messages.filter((m) => m.triage?.needs_reply).length);
  hasMore = $derived(this.hits === null && (this.currentMailbox?.exists_count ?? 0) > this.messages.length);

  // ---------------------------------------------------------------- lifecycle

  async init() {
    await this.#wireEvents();
    await this.refreshAccounts();
    void this.loadAiConfig();
    if (this.accounts.length === 0) {
      this.settingsOpen = true;
    } else {
      await this.selectAccount(this.accounts[0].id);
    }
    void this.refreshOutbox();
    void this.refreshDrafts();
    this.booted = true;
  }

  async #wireEvents() {
    await listen<SyncReport>("mail://synced", (e) => this.#onSynced(e.payload));
    await listen<{ account: string; error: string }>("mail://watch-error", (e) => {
      const name = this.accounts.find((a) => a.id === e.payload.account)?.name ?? "account";
      this.toast("error", `Live updates stopped for ${name}: ${e.payload.error}`);
    });
    await listen("outbox://changed", () => void this.refreshOutbox());
  }

  #onSynced(r: SyncReport) {
    const acct = this.accounts.find((a) => a.name === r.account);
    if (!acct) return;
    // Coalesce bursts of events into one refresh.
    if (this.#refreshTimer) clearTimeout(this.#refreshTimer);
    this.#refreshTimer = setTimeout(async () => {
      if (acct.id === this.accountId) {
        await this.refreshMailboxes();
        if (r.mailbox === this.mailbox && this.hits === null) await this.#reloadHead();
      }
    }, 250);
    if (r.stored > 0 && r.mailbox.toUpperCase() === "INBOX" && this.booted) {
      this.toast("info", `${r.stored} new in ${r.account}`);
    }
  }

  // ---------------------------------------------------------------- toasts

  toast(kind: Toast["kind"], text: string, action?: Toast["action"]) {
    const id = ++this.#toastId;
    this.toasts = [...this.toasts, { id, kind, text, action }];
    setTimeout(() => this.dismissToast(id), kind === "error" ? 9000 : 5000);
  }

  dismissToast(id: number) {
    this.toasts = this.toasts.filter((t) => t.id !== id);
  }

  setPref<K extends keyof Prefs>(key: K, value: Prefs[K]) {
    this.prefs[key] = value;
    try {
      localStorage.setItem(PREFS_KEY, JSON.stringify(this.prefs));
    } catch {
      /* private mode or full storage: the preference just does not persist */
    }
  }

  // ---------------------------------------------------------------- accounts and folders

  async refreshAccounts() {
    this.accounts = await api.accounts();
  }

  async selectAccount(id: string, mailbox = "INBOX") {
    this.accountId = id;
    this.mailbox = mailbox;
    this.clearSearch();
    this.#clearMessage();
    await this.refreshMailboxes();
    // A brand new account has no folders until its first sync.
    if (this.mailboxes.length === 0) await this.sync();
    await this.#loadList();
  }

  async refreshMailboxes() {
    if (!this.accountId) return;
    const id = this.accountId;
    try {
      const list = await api.mailboxes(id);
      if (id === this.accountId) this.mailboxes = list;
    } catch (e) {
      this.toast("error", errText(e));
    }
  }

  async selectMailbox(name: string) {
    this.mailbox = name;
    this.clearSearch();
    this.#clearMessage();
    await this.#loadList();
    if (this.currentMailbox && this.currentMailbox.last_synced_at === null) await this.sync();
  }

  async addAccount(input: AddAccountInput): Promise<{ warning?: string }> {
    const acct = await api.addAccount(input);
    try {
      const report = await api.testAccount(acct.id);
      await shell.restartWatchers();
      await this.refreshAccounts();
      await this.selectAccount(acct.id);
      return { warning: report.smtp_error ? `Mail can be read, but sending is not set up: ${report.smtp_error}` : undefined };
    } catch (e) {
      // Wrong password or host: do not leave a broken account behind.
      await api.removeAccount(acct.id).catch(() => {});
      await this.refreshAccounts();
      throw e;
    }
  }

  async removeAccount(id: string) {
    await api.removeAccount(id);
    await shell.restartWatchers();
    await this.refreshAccounts();
    if (this.accountId === id) {
      this.accountId = null;
      this.mailboxes = [];
      this.messages = [];
      this.#clearMessage();
      if (this.accounts.length) await this.selectAccount(this.accounts[0].id);
    }
  }

  // ---------------------------------------------------------------- message list

  async #loadList() {
    if (!this.accountId) return;
    const token = ++this.#listToken;
    this.loadingList = true;
    try {
      const rows = await api.messages(this.accountId, this.mailbox, PAGE, 0);
      if (token === this.#listToken) this.messages = rows;
    } catch (e) {
      if (token === this.#listToken) {
        this.messages = [];
        // "mailbox not found" just means it has not been synced yet.
        if (!/not found/i.test(errText(e))) this.toast("error", errText(e));
      }
    } finally {
      if (token === this.#listToken) this.loadingList = false;
    }
  }

  /** Reload the first page after new mail arrives, keeping whatever is selected. */
  async #reloadHead() {
    if (!this.accountId) return;
    const token = this.#listToken;
    const keep = Math.max(this.messages.length, PAGE);
    try {
      const rows = await api.messages(this.accountId, this.mailbox, keep, 0);
      if (token === this.#listToken) this.messages = rows;
    } catch {
      /* the next event will try again */
    }
  }

  async loadMore() {
    if (!this.accountId || this.loadingList) return;
    this.loadingList = true;
    try {
      let rows = await api.messages(this.accountId, this.mailbox, PAGE, this.messages.length);
      if (rows.length === 0) {
        // Everything cached is on screen: reach further back on the server.
        await api.sync(this.accountId, this.mailbox, 50, PAGE);
        await this.refreshMailboxes();
        rows = await api.messages(this.accountId, this.mailbox, PAGE, this.messages.length);
      }
      this.messages = [...this.messages, ...rows];
    } catch (e) {
      this.toast("error", errText(e));
    } finally {
      this.loadingList = false;
    }
  }

  async sync() {
    if (!this.accountId || this.syncing) return;
    this.syncing = true;
    try {
      await api.sync(this.accountId, this.mailbox);
      await this.refreshMailboxes();
      await this.#reloadHead();
    } catch (e) {
      this.toast("error", `Sync failed: ${errText(e)}`);
    } finally {
      this.syncing = false;
    }
  }

  // ---------------------------------------------------------------- search

  async runSearch() {
    const q = this.query.trim();
    if (!q) return this.clearSearch();
    this.searching = true;
    try {
      this.hits = await api.search(q, this.searchAll ? undefined : (this.accountId ?? undefined), 100);
    } catch (e) {
      this.toast("error", errText(e));
    } finally {
      this.searching = false;
    }
  }

  clearSearch() {
    this.query = "";
    this.hits = null;
  }

  // ---------------------------------------------------------------- reading

  #clearMessage() {
    this.#openToken++;
    this.selectedUid = null;
    this.detail = null;
    this.html = null;
    this.thread = [];
    this.allowImages = false;
    this.messageError = null;
    this.loadingMessage = false;
    this.proposals = [];
    this.aiErrors = [];
    this.analysing = false;
  }

  /** Open a message. `hit` carries its own account/folder when it comes from a cross-account search. */
  async openMessage(row: MessageRow | SearchHit) {
    const hit = row as Partial<SearchHit>;
    const mailbox = hit.mailbox ?? this.mailbox;
    const mb = this.mailboxes.find((m) => m.id === row.mailbox_id);
    let accountId = this.accountId;
    if (hit.mailbox && !mb) {
      // A result from another account: switch there first, keeping the search results.
      const owner = await this.#accountOfMailbox(row.mailbox_id);
      if (owner && owner !== this.accountId) {
        const saved = { q: this.query, hits: this.hits };
        await this.selectAccount(owner, mailbox);
        this.query = saved.q;
        this.hits = saved.hits;
        accountId = owner;
      }
    } else if (hit.mailbox && hit.mailbox !== this.mailbox) {
      this.mailbox = hit.mailbox;
    }
    if (!accountId) return;

    const token = ++this.#openToken;
    this.selectedUid = row.uid;
    this.allowImages = false;
    this.html = null;
    this.thread = [];
    this.proposals = [];
    this.aiErrors = [];
    this.analysing = false;
    this.messageError = null;
    this.loadingMessage = true;
    try {
      const d = await api.message(accountId, mailbox, row.uid);
      if (token !== this.#openToken) return;
      this.detail = d;
      const [html, thread] = await Promise.all([
        d.body_html ? api.messageHtml(accountId, mailbox, row.uid, false) : Promise.resolve(null),
        d.thread_id ? api.thread(accountId, d.thread_id) : Promise.resolve([] as MessageRow[]),
      ]);
      if (token !== this.#openToken) return;
      this.html = html;
      this.thread = thread.length > 1 ? thread : [];
      if (this.prefs.markReadOnOpen && isUnread(d)) void this.setSeen(row.uid, true);
      if (this.aiEnabled) void this.analyze(false);
    } catch (e) {
      if (token === this.#openToken) {
        this.detail = null;
        this.messageError = errText(e);
      }
    } finally {
      if (token === this.#openToken) this.loadingMessage = false;
    }
  }

  async #accountOfMailbox(mailboxId: number): Promise<string | null> {
    for (const a of this.accounts) {
      if (a.id === this.accountId) continue;
      const list = await api.mailboxes(a.id).catch(() => [] as Mailbox[]);
      if (list.some((m) => m.id === mailboxId)) return a.id;
    }
    return null;
  }

  async loadRemoteImages() {
    if (!this.accountId || !this.detail) return;
    const token = this.#openToken;
    try {
      const html = await api.messageHtml(this.accountId, this.detail.mailbox, this.detail.uid, true);
      if (token !== this.#openToken) return;
      this.html = html;
      this.allowImages = true;
    } catch (e) {
      this.toast("error", errText(e));
    }
  }

  /** Move the selection by `delta` rows (j/k). */
  async step(delta: number) {
    const rows = this.rows;
    if (rows.length === 0) return;
    const i = rows.findIndex((r) => r.uid === this.selectedUid);
    const next = rows[Math.min(rows.length - 1, Math.max(0, i < 0 ? (delta > 0 ? 0 : rows.length - 1) : i + delta))];
    if (next && next.uid !== this.selectedUid) await this.openMessage(next);
  }

  // ---------------------------------------------------------------- AI

  async loadAiConfig() {
    try {
      this.aiConfig = await api.aiConfig();
    } catch {
      /* AI is optional: an unreadable setting just leaves it off */
    }
  }

  async refreshAiStatus() {
    try {
      this.aiStatus = await api.aiStatus();
      this.aiConfig = this.aiStatus.config;
    } catch (e) {
      this.toast("error", errText(e));
    }
  }

  async saveAiConfig(c: AiConfig) {
    this.aiConfig = await api.aiSetConfig(c);
    await this.refreshAiStatus();
    // Turning AI on should take effect for the message already open.
    if (this.aiEnabled && this.detail && this.proposals.length === 0 && !this.detail.triage) void this.analyze(false);
  }

  #patchRow(row: MessageRow) {
    const same = (m: MessageRow) => m.uid === row.uid && m.mailbox_id === row.mailbox_id;
    this.messages = this.messages.map((m) => (same(m) ? { ...m, triage: row.triage, pending_events: row.pending_events, flags: m.flags } : m));
    if (this.hits) this.hits = this.hits.map((m) => (same(m) ? { ...m, triage: row.triage, pending_events: row.pending_events } : m));
    if (this.detail && same(this.detail)) this.detail = { ...this.detail, triage: row.triage, pending_events: row.pending_events };
  }

  /** Triage the open message and look for events. Cached results come back instantly. */
  async analyze(force: boolean) {
    if (!this.aiEnabled || !this.accountId || !this.detail) return;
    const token = this.#openToken;
    const { uid, mailbox } = this.detail;
    this.analysing = true;
    this.aiErrors = [];
    try {
      const a = await api.aiAnalyze(this.accountId, mailbox, uid, force);
      if (token !== this.#openToken) return;
      this.proposals = a.proposals;
      this.aiErrors = a.errors;
      this.#patchRow(a.message);
    } catch (e) {
      if (token === this.#openToken) this.aiErrors = [errText(e)];
    } finally {
      if (token === this.#openToken) this.analysing = false;
    }
  }

  async acceptProposal(id: number): Promise<boolean> {
    try {
      const ev = await api.aiAccept(id);
      this.proposals = this.proposals.map((p) => (p.id === id ? { ...p, state: "accepted" } : p));
      this.#adjustPending(-1);
      this.toast("success", `Added “${ev.title}” to your calendar`, { label: "View", run: () => (this.view = "calendar") });
      return true;
    } catch (e) {
      this.toast("error", errText(e));
      return false;
    }
  }

  async dismissProposal(id: number) {
    try {
      await api.aiDismiss(id);
      this.proposals = this.proposals.map((p) => (p.id === id ? { ...p, state: "dismissed" } : p));
      this.#adjustPending(-1);
    } catch (e) {
      this.toast("error", errText(e));
    }
  }

  #adjustPending(delta: number) {
    if (!this.detail) return;
    const n = Math.max(0, this.detail.pending_events + delta);
    const uid = this.detail.uid;
    this.detail = { ...this.detail, pending_events: n };
    this.messages = this.messages.map((m) => (m.uid === uid && m.mailbox_id === this.detail?.mailbox_id ? { ...m, pending_events: n } : m));
  }

  /** Analyse the newest messages of this folder that have not been analysed yet. */
  async analyzeFolder() {
    if (!this.accountId || this.analysingFolder || !this.aiEnabled) return;
    this.analysingFolder = true;
    try {
      const r = await api.aiTriageFolder(this.accountId, this.mailbox, 30);
      await this.#reloadHead();
      if (r.stopped_early) this.toast("error", `The local model is not reachable. ${r.errors[0] ?? ""}`);
      else this.toast("success", r.analyzed === 0 ? "Everything recent is already analysed" : `Analysed ${r.analyzed} message${r.analyzed === 1 ? "" : "s"}${r.failed ? `, ${r.failed} failed` : ""}`);
    } catch (e) {
      this.toast("error", errText(e));
    } finally {
      this.analysingFolder = false;
    }
  }

  // ---------------------------------------------------------------- AI drafting

  async draftWithAi() {
    const c = this.compose;
    if (!c?.replyTo || c.ai.busy) return;
    c.ai.error = null;
    if (c.ai.provider === "cloud") {
      // Cloud: show exactly what would be sent and wait for the user's confirmation.
      c.ai.busy = true;
      try {
        c.ai.preview = await api.aiDraftPreview(this.#draftArgs(c, undefined));
      } catch (e) {
        c.ai.error = errText(e);
      } finally {
        c.ai.busy = false;
      }
      return;
    }
    await this.#runDraft(c, undefined);
  }

  async confirmCloudDraft() {
    const c = this.compose;
    if (c?.ai.preview) await this.#runDraft(c, c.ai.preview.hash);
  }

  #draftArgs(c: Compose, hash: string | undefined) {
    const r = c.replyTo!;
    return {
      account: r.account, mailbox: r.mailbox, uid: r.uid, instruction: c.ai.instruction,
      existing_draft: splitReplyBody(c.body).typed, provider: c.ai.provider, confirm_hash: hash,
    };
  }

  async #runDraft(c: Compose, hash: string | undefined) {
    c.ai.busy = true;
    try {
      const r = await api.aiDraft(this.#draftArgs(c, hash));
      const { quoted } = splitReplyBody(c.body);
      c.ai.undo = c.body;
      c.body = r.draft + (quoted ? `\n\n${quoted}` : "\n");
      c.dirty = true;
      c.ai.preview = null;
    } catch (e) {
      c.ai.error = errText(e);
    } finally {
      c.ai.busy = false;
      void this.refreshAiStatus();
    }
  }

  undoDraft() {
    const c = this.compose;
    if (c?.ai.undo !== null && c?.ai.undo !== undefined) {
      c.body = c.ai.undo;
      c.ai.undo = null;
    }
  }

  // ---------------------------------------------------------------- flags

  #patchFlags(uid: number, flags: string) {
    this.messages = this.messages.map((m) => (m.uid === uid ? { ...m, flags } : m));
    if (this.hits) this.hits = this.hits.map((m) => (m.uid === uid ? { ...m, flags } : m));
    if (this.detail?.uid === uid) this.detail = { ...this.detail, flags };
  }

  async #setFlag(uid: number, flag: "seen" | "flagged", on: boolean) {
    const token = flag === "seen" ? "\\Seen" : "\\Flagged";
    const row = this.rows.find((r) => r.uid === uid) ?? this.detail;
    if (!row || !this.accountId) return;
    const before = row.flags;
    const set = new Set(before.split(/\s+/).filter(Boolean));
    if (on) set.add(token);
    else set.delete(token);
    this.#patchFlags(uid, [...set].sort().join(" "));
    try {
      const updated = await api.setFlag(this.accountId, (row as Partial<SearchHit>).mailbox ?? this.detail?.mailbox ?? this.mailbox, uid, flag, on);
      this.#patchFlags(uid, updated.flags);
      void this.refreshMailboxes();
    } catch (e) {
      this.#patchFlags(uid, before);
      this.toast("error", `Could not update the message: ${errText(e)}`);
    }
  }

  setSeen = (uid: number, on: boolean) => this.#setFlag(uid, "seen", on);
  setStarred = (uid: number, on: boolean) => this.#setFlag(uid, "flagged", on);

  // ---------------------------------------------------------------- attachments and links

  async saveAttachment(index: number) {
    if (!this.accountId || !this.detail) return;
    try {
      const dir = await shell.downloadDir();
      const saved = await api.saveAttachment(this.accountId, this.detail.mailbox, this.detail.uid, index, dir);
      this.toast("success", `Saved ${saved.filename}`, { label: "Show", run: () => void shell.reveal(saved.path) });
    } catch (e) {
      this.toast("error", `Could not save attachment: ${errText(e)}`);
    }
  }

  async openLink(url: string) {
    try {
      await shell.openExternal(url);
    } catch (e) {
      this.toast("error", errText(e));
    }
  }

  // ---------------------------------------------------------------- compose and outbox

  openCompose(init?: Partial<ComposeInit>) {
    if (this.compose?.sending) return;
    const accountId = this.accountId ?? this.accounts[0]?.id;
    if (!accountId) {
      this.toast("error", "Add an account first.");
      return;
    }
    this.compose = {
      accountId,
      to: (init?.to ?? []).join(", "),
      cc: (init?.cc ?? []).join(", "),
      bcc: "",
      showCc: (init?.cc?.length ?? 0) > 0,
      subject: init?.subject ?? "",
      body: init?.body ?? "",
      attachments: [],
      inReplyTo: init?.inReplyTo,
      references: init?.references ?? [],
      sending: false,
      error: null,
      dirty: false,
      replyTo: (init as { replyTo?: Compose["replyTo"] } | undefined)?.replyTo,
      ai: { open: false, instruction: "", provider: this.aiConfig?.draft_provider ?? "local", busy: false, error: null, preview: null, undo: null },
    };
  }

  reply() {
    if (!this.detail || !this.accountId) return;
    this.openCompose({ ...buildReply(this.detail), replyTo: { account: this.accountId, mailbox: this.detail.mailbox, uid: this.detail.uid } } as Partial<ComposeInit>);
  }

  async replyAll() {
    const d = this.detail;
    if (!d || !this.accountId) return;
    try {
      const who = await api.replyAll(this.accountId, d.mailbox, d.uid);
      this.openCompose({ ...buildReplyAll(d, who), replyTo: { account: this.accountId, mailbox: d.mailbox, uid: d.uid } } as Partial<ComposeInit>);
    } catch (e) {
      this.toast("error", errText(e));
    }
  }

  async attachFiles() {
    if (!this.compose) return;
    const picked = await pickFiles({ multiple: true, directory: false });
    if (!picked) return;
    const paths = Array.isArray(picked) ? picked : [picked];
    const have = new Set(this.compose.attachments.map((a) => a.path));
    for (const p of paths) {
      if (!have.has(p)) this.compose.attachments.push({ path: p, name: p.split(/[\\/]/).pop() ?? p });
    }
    this.compose.dirty = true;
  }

  async send() {
    const c = this.compose;
    if (!c || c.sending) return;
    const to = parseRecipients(c.to);
    const cc = parseRecipients(c.cc);
    const bcc = parseRecipients(c.bcc);
    if (to.length + cc.length + bcc.length === 0) {
      c.error = "Add at least one recipient.";
      return;
    }
    c.sending = true;
    c.error = null;
    try {
      const out = await api.send({
        account: c.accountId, to, cc, bcc, subject: c.subject, body: c.body,
        in_reply_to: c.inReplyTo, references: c.references, attachments: c.attachments.map((a) => a.path),
      });
      if (out.item.state === "sent") {
        this.toast("success", "Message sent");
        this.#finishDraft(c);
      } else if (out.item.state === "queued") {
        this.toast("info", `Could not reach the mail server yet. It is queued and will be retried. (${out.item.last_error ?? ""})`);
        this.#finishDraft(c);
      } else {
        c.error = out.item.last_error ?? "The server rejected the message.";
        c.sending = false;
      }
    } catch (e) {
      c.error = errText(e);
      c.sending = false;
    }
    void this.refreshOutbox();
  }

  /** The message left the compose window for good (sent or queued): its draft, if any, is done. */
  #finishDraft(c: Compose) {
    this.compose = null;
    if (c.draftId) void this.deleteDraft(c.draftId);
  }

  async refreshDrafts() {
    try {
      this.drafts = await api.drafts();
    } catch {
      /* drafts are secondary: stay quiet if they are momentarily unavailable */
    }
  }

  /** Keep the open compose window as a draft. Returns true when it was saved. */
  async saveDraft(): Promise<boolean> {
    const c = this.compose;
    if (!c) return false;
    const account = this.accounts.find((a) => a.id === c.accountId);
    if (!account) return false;
    try {
      const saved = await api.draftSave({
        id: c.draftId, account: account.name, to: parseRecipients(c.to), cc: parseRecipients(c.cc), bcc: parseRecipients(c.bcc),
        subject: c.subject, body: c.body, in_reply_to: c.inReplyTo ?? null, references: c.references,
        attachments: c.attachments.map((a) => a.path), reply_to: c.replyTo ? [c.replyTo.mailbox, c.replyTo.uid] : null,
      });
      c.draftId = saved.id;
      c.dirty = false;
      void this.refreshDrafts();
      return true;
    } catch (e) {
      this.toast("error", `Could not save the draft: ${errText(e)}`);
      return false;
    }
  }

  openDraft(d: Draft) {
    if (this.compose?.sending) return;
    const account = this.accounts.find((a) => a.name === d.account);
    if (!account) {
      this.toast("error", `The account "${d.account}" no longer exists.`);
      return;
    }
    this.openCompose({ to: d.to, cc: d.cc, subject: d.subject, body: d.body, inReplyTo: d.in_reply_to ?? undefined, references: d.references });
    const c = this.compose!;
    c.accountId = account.id;
    c.bcc = d.bcc.join(", ");
    c.draftId = d.id;
    c.attachments = d.attachments.map((p) => ({ path: p, name: p.split(/[\\/]/).pop() ?? p }));
    if (d.reply_to) c.replyTo = { account: account.id, mailbox: d.reply_to[0], uid: d.reply_to[1] };
    c.showCc = d.cc.length > 0 || d.bcc.length > 0;
    this.draftsOpen = false;
  }

  async deleteDraft(id: string) {
    try {
      await api.draftDelete(id);
    } catch (e) {
      this.toast("error", errText(e));
    }
    void this.refreshDrafts();
  }

  async refreshOutbox() {
    try {
      this.outbox = await api.outbox(false);
    } catch {
      /* the outbox is secondary: stay quiet if it is momentarily unavailable */
    }
  }
}

export const app = new App();
