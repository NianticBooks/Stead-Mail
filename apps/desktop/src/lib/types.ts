// Shapes returned by the mailcore JSON-RPC API (serde output of the Rust types).

export type Provider = "icloud" | "gmail" | "custom";

export interface ServerConfig {
  host: string;
  port: number;
  starttls: boolean;
  username: string;
}

export interface Account {
  id: string;
  name: string;
  email: string;
  provider: Provider;
  auth: string;
  imap: ServerConfig;
  smtp: ServerConfig;
}

export interface Mailbox {
  id: number;
  account_id: string;
  name: string;
  delimiter: string | null;
  attributes: string;
  uidvalidity: number | null;
  uidnext: number | null;
  exists_count: number | null;
  last_synced_at: string | null;
  unseen: number | null;
}

export interface MessageRow {
  id: number;
  mailbox_id: number;
  uid: number;
  message_id: string | null;
  in_reply_to: string | null;
  references: string | null;
  subject: string | null;
  from_name: string | null;
  from_addr: string | null;
  to_addrs: string | null;
  date: string | null;
  internal_date: string | null;
  size: number | null;
  flags: string;
  thread_id: string | null;
  preview: string | null;
  has_attachments: boolean;
  body_cached: boolean;
  triage: TriageBrief | null;
  pending_events: number;
}

export type Category = "conversation" | "scheduling" | "transaction" | "notification" | "newsletter" | "other";

export interface TriageBrief {
  category: Category;
  needs_reply: boolean;
  urgency: "low" | "normal" | "high";
  summary: string;
  /** The message tried to give instructions to an AI; it was not shown to a model. */
  suspicious: boolean;
}

export interface Attachment {
  part_index: number;
  filename: string | null;
  content_type: string;
  size: number;
  content_id: string | null;
  inline: boolean;
}

export interface MessageDetail extends MessageRow {
  mailbox: string;
  body_text: string | null;
  body_html: string | null;
  attachments: Attachment[];
}

export interface SanitizedHtml {
  html: string;
  blocked_images: number;
  remote_images_allowed: boolean;
}

export interface SearchHit extends MessageRow {
  mailbox: string;
  snippet: string;
}

export interface SyncReport {
  /** The account's display name, not its id. */
  account: string;
  mailbox: string;
  exists: number;
  uidvalidity: number | null;
  uidvalidity_changed: boolean;
  fetched: number;
  stored: number;
  backfilled: number;
  flags_updated: number;
  expunged: number;
  cached_total: number;
  mailboxes_seen: number;
  unseen: number | null;
}

export interface OutboxItem {
  id: string;
  account_id: string;
  message_id: string;
  from: string;
  rcpts: string[];
  subject: string;
  size: number;
  state: "queued" | "sending" | "sent" | "failed" | "cancelled";
  attempts: number;
  next_attempt_at: number;
  last_error: string | null;
  created_at: string;
  sent_at: string | null;
}

export interface SendOutcome {
  item: OutboxItem;
  delivered: boolean;
}

export interface ConnectionReport {
  account: string;
  imap_host: string;
  mailboxes: { name: string }[];
  smtp_host: string;
  smtp_error: string | null;
}

export interface OAuthIdentity {
  id: string;
  provider: string;
  email: string;
  scopes: string[];
}

export interface CalendarSummary {
  id: string;
  summary: string;
  primary: boolean;
  access_role: string;
  time_zone: string | null;
}

export interface SavedAttachment {
  path: string;
  filename: string;
  content_type: string;
  bytes: number;
}

export interface AddAccountInput {
  name: string;
  email: string;
  provider: Provider;
  password: string;
  imap_host?: string;
  imap_port?: number;
  smtp_host?: string;
  smtp_port?: number;
}

export interface EventProposal {
  id: number;
  message_id: number;
  /** "ics" = an invitation file in the message; "ai" = read from the text by a model. */
  source: "ics" | "ai";
  method: string | null;
  ics_uid: string | null;
  title: string;
  start_utc: string;
  end_utc: string | null;
  all_day: boolean;
  tz: string | null;
  location: string | null;
  attendees: string[];
  rrule: string | null;
  confidence: number | null;
  quote: string | null;
  model: string | null;
  state: "pending" | "accepted" | "dismissed";
}

export interface CalendarEvent {
  id: number;
  title: string;
  start_utc: string;
  end_utc: string | null;
  all_day: boolean;
  tz: string | null;
  location: string | null;
  attendees: string[];
}

export type ProviderKind = "local" | "cloud";

export interface AiConfig {
  enabled: boolean;
  ollama_url: string;
  local_model: string;
  num_ctx: number;
  extract_provider: ProviderKind;
  draft_provider: ProviderKind;
  cloud_enabled: boolean;
  cloud_extract_model: string;
  cloud_draft_model: string;
  timezone: string;
  min_event_confidence: number;
}

export interface AiUsage {
  provider: ProviderKind;
  calls: number;
  bytes_out: number;
  bytes_in: number;
}

export interface AiStatus {
  config: AiConfig;
  local: { url: string; model: string; reachable: boolean; model_installed: boolean; installed_models: string[]; error: string | null };
  cloud_key_present: boolean;
  usage: AiUsage[];
}

export interface AiCall {
  id: number;
  at: string;
  provider: ProviderKind;
  model: string;
  task: string;
  bytes_out: number;
  bytes_in: number;
  millis: number;
  ok: boolean;
  error: string | null;
}

export interface AuditReport {
  usage: AiUsage[];
  calls: AiCall[];
}

export interface Analysis {
  message: MessageRow;
  proposals: EventProposal[];
  errors: string[];
}

export interface CloudPreview {
  model: string;
  system: string;
  user: string;
  bytes: number;
  hash: string;
}

export interface DraftResult {
  draft: string;
  provider: ProviderKind;
  model: string;
  millis: number;
}
