import { invoke } from "@tauri-apps/api/core";
import type {
  Account, AddAccountInput, AiConfig, AiStatus, Analysis, AuditReport, CalendarEvent, CloudPreview, DraftResult, EventProposal, ProviderKind, CalendarSummary, ConnectionReport, Mailbox, MessageDetail, MessageRow, OAuthIdentity,
  OutboxItem, SanitizedHtml, SavedAttachment, SearchHit, SendOutcome, SyncReport,
} from "./types";

/** One call into the core's JSON-RPC surface. Rejects with the core's own error message. */
export function rpc<T>(method: string, params?: Record<string, unknown>): Promise<T> {
  return invoke<T>("rpc", { method, params: params ?? null });
}

export interface SendInput {
  account: string;
  to: string[];
  cc: string[];
  bcc: string[];
  subject: string;
  body: string;
  in_reply_to?: string;
  references: string[];
  attachments: string[];
}

export interface DraftArgs {
  account: string;
  mailbox: string;
  uid: number;
  instruction: string;
  existing_draft: string;
  provider: ProviderKind;
  confirm_hash?: string;
}

export const api = {
  accounts: () => rpc<Account[]>("accounts.list"),
  addAccount: (a: AddAccountInput) => rpc<Account>("accounts.add", { ...a }),
  removeAccount: (account: string) => rpc<Account>("accounts.remove", { account }),
  testAccount: (account: string) => rpc<ConnectionReport>("accounts.test", { account }),

  mailboxes: (account: string) => rpc<Mailbox[]>("mailboxes.list", { account }),
  messages: (account: string, mailbox: string, limit: number, offset: number) =>
    rpc<MessageRow[]>("messages.list", { account, mailbox, limit, offset }),
  message: (account: string, mailbox: string, uid: number) => rpc<MessageDetail>("message.get", { account, mailbox, uid }),
  messageHtml: (account: string, mailbox: string, uid: number, allowRemoteImages: boolean) =>
    rpc<SanitizedHtml | null>("message.html", { account, mailbox, uid, allow_remote_images: allowRemoteImages }),
  setFlag: (account: string, mailbox: string, uid: number, flag: "seen" | "flagged", on: boolean) =>
    rpc<MessageRow>("message.set_flag", { account, mailbox, uid, flag, on }),
  thread: (account: string, threadId: string) => rpc<MessageRow[]>("thread", { account, thread_id: threadId }),
  search: (query: string, account: string | undefined, limit: number) => rpc<SearchHit[]>("search", { query, account, limit }),

  sync: (account: string, mailbox: string, limit = 50, backfill = 0) => rpc<SyncReport>("sync", { account, mailbox, limit, backfill }),
  send: (s: SendInput) => rpc<SendOutcome>("send", { ...s }),
  outbox: (all = false) => rpc<OutboxItem[]>("outbox.list", { all, limit: 50 }),
  outboxRetry: (id: string) => rpc<OutboxItem>("outbox.retry", { id }),
  outboxCancel: (id: string) => rpc<OutboxItem>("outbox.cancel", { id }),
  outboxFlush: () => rpc<{ attempted: number; sent: number; retrying: number; failed: number }>("outbox.flush"),
  saveAttachment: (account: string, mailbox: string, uid: number, index: number, dir: string) =>
    rpc<SavedAttachment>("attachment.save", { account, mailbox, uid, index, dir }),

  aiStatus: () => rpc<AiStatus>("ai.status"),
  aiConfig: () => rpc<AiConfig>("ai.config"),
  aiSetConfig: (c: AiConfig) => rpc<AiConfig>("ai.set_config", { ...c }),
  aiSetKey: (key: string) => rpc<{ stored: boolean }>("ai.set_cloud_key", { key }),
  aiClearKey: () => rpc<{ stored: boolean }>("ai.clear_cloud_key"),
  aiAnalyze: (account: string, mailbox: string, uid: number, force = false) => rpc<Analysis>("ai.analyze", { account, mailbox, uid, force }),
  aiTriageFolder: (account: string, mailbox: string, limit = 30) =>
    rpc<{ analyzed: number; skipped: number; failed: number; stopped_early: boolean; errors: string[] }>("ai.triage_folder", { account, mailbox, limit }),
  aiProposals: (account: string, mailbox: string, uid: number) => rpc<EventProposal[]>("ai.proposals", { account, mailbox, uid }),
  aiAccept: (id: number) => rpc<CalendarEvent>("ai.accept", { id }),
  aiDismiss: (id: number) => rpc<EventProposal>("ai.dismiss", { id }),
  aiDraftPreview: (r: DraftArgs) => rpc<CloudPreview | null>("ai.draft_preview", { ...r }),
  aiDraft: (r: DraftArgs) => rpc<DraftResult>("ai.draft", { ...r }),
  aiAudit: (limit = 20) => rpc<AuditReport>("ai.audit", { limit }),

  googleConfigured: () => rpc<{ configured: boolean }>("google.configured"),
  googleConfigure: (client_id: string, client_secret: string | null) => rpc<{ configured: boolean }>("google.configure", { client_id, client_secret }),
  googleIdentities: () => rpc<OAuthIdentity[]>("google.identities"),
  googleCalendars: (account: string) => rpc<CalendarSummary[]>("google.calendars", { account }),
  googleLogout: (account: string) => rpc<OAuthIdentity>("google.logout", { account }),
  googleLogin: () => invoke<OAuthIdentity>("google_login"),
};

export const shell = {
  openExternal: (url: string) => invoke<void>("open_external", { url }),
  reveal: (path: string) => invoke<void>("reveal_file", { path }),
  downloadDir: () => invoke<string>("download_dir"),
  restartWatchers: () => invoke<void>("restart_watchers"),
};
