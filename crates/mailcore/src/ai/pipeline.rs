//! The AI features as operations on the mailbox: settings, status, analysing a message, batches,
//! drafting with the cloud-consent gate, and the audit trail.

use serde::{Deserialize, Serialize};

use super::draft::{self, CloudPreview, DraftInput};
use super::events;
use super::llm::{JsonRequest, Llm, LlmResponse};
use super::prompt::{MailInput, date_snippets};
use super::triage::{self, Category, Triage};
use super::{AiConfig, Provider};
use crate::api::Api;
use crate::error::{Error, Result};
use crate::store::{AiCall, AiUsage, EventProposal, MessageDetail, MessageRow};

const SETTINGS_KEY: &str = "ai.config";
const CLOUD_KEY: &str = "ai/anthropic_api_key";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalStatus {
    pub url: String,
    pub model: String,
    pub reachable: bool,
    pub model_installed: bool,
    pub installed_models: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiStatus {
    pub config: AiConfig,
    pub local: LocalStatus,
    pub cloud_key_present: bool,
    /// Calls and bytes per provider, last 30 days. The cloud row is what has left this computer.
    pub usage: Vec<AiUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Analysis {
    pub message: MessageRow,
    pub triage: Option<Triage>,
    pub proposals: Vec<EventProposal>,
    /// Parts that could not run (model unreachable, bad answer). The rest still completed.
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BatchReport {
    pub analyzed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub stopped_early: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DraftRequest {
    pub account: String,
    pub mailbox: String,
    pub uid: u32,
    #[serde(default)]
    pub instruction: String,
    #[serde(default)]
    pub existing_draft: String,
    /// Defaults to the configured drafting provider.
    #[serde(default)]
    pub provider: Option<Provider>,
    /// For cloud drafts: the hash of the preview the user was shown.
    #[serde(default)]
    pub confirm_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftResult {
    pub draft: String,
    pub provider: Provider,
    pub model: String,
    pub millis: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReport {
    pub usage: Vec<AiUsage>,
    pub calls: Vec<AiCall>,
}

/// Folders that hold mail the user wrote: Sent and Drafts, by special-use flag or by name. Mail there
/// can carry an alias address the account does not know about, so the folder is the better signal.
pub fn is_outgoing_folder(name: &str, attributes: &str) -> bool {
    let attrs = attributes.to_ascii_lowercase();
    if attrs.split_whitespace().any(|a| a == "sent" || a == "drafts") {
        return true;
    }
    let last = name.rsplit('/').next().unwrap_or(name).to_ascii_lowercase();
    matches!(last.as_str(), "sent" | "sent items" | "sent messages" | "sent mail" | "outbox" | "drafts" | "draft")
}

fn mail_input(d: &MessageDetail) -> MailInput {
    let from = match (&d.row.header.from_name, &d.row.header.from_addr) {
        (Some(n), Some(a)) => format!("{n} <{a}>"),
        (None, Some(a)) => a.clone(),
        (Some(n), None) => n.clone(),
        _ => String::new(),
    };
    let date = d
        .row
        .header
        .date
        .as_deref()
        .or(d.row.header.internal_date.as_deref())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&chrono::Utc));
    MailInput { from, subject: d.row.header.subject.clone().unwrap_or_default(), date, body: d.body_text.clone().or_else(|| d.row.preview.clone()).unwrap_or_default() }
}

impl Api {
    // ------------------------------------------------------------ settings

    pub fn ai_config(&self) -> Result<AiConfig> {
        match self.store().get_setting(SETTINGS_KEY)? {
            Some(json) => serde_json::from_str(&json).map_err(|e| Error::Other(format!("stored AI settings are corrupt: {e}"))),
            None => Ok(AiConfig::default()),
        }
    }

    pub fn ai_set_config(&self, cfg: AiConfig) -> Result<AiConfig> {
        cfg.validate().map_err(Error::Invalid)?;
        self.store().set_setting(SETTINGS_KEY, &serde_json::to_string(&cfg).map_err(|e| Error::Other(e.to_string()))?)?;
        Ok(cfg)
    }

    pub fn ai_set_cloud_key(&self, key: &str) -> Result<()> {
        let key = key.trim();
        if key.len() < 20 || key.chars().any(char::is_whitespace) {
            return Err(Error::Invalid("that does not look like an API key".into()));
        }
        self.secrets.set(CLOUD_KEY, key)
    }

    pub fn ai_clear_cloud_key(&self) -> Result<()> {
        self.secrets.delete(CLOUD_KEY)
    }

    fn cloud_key(&self) -> Result<Option<String>> {
        self.secrets.get(CLOUD_KEY)
    }

    pub async fn ai_status(&self) -> Result<AiStatus> {
        let config = self.ai_config()?;
        let url = format!("{}/api/tags", config.ollama_url.trim_end_matches('/'));
        let mut local = LocalStatus { url: config.ollama_url.clone(), model: config.local_model.clone(), reachable: false, model_installed: false, installed_models: vec![], error: None };
        match self.http.get(&url).timeout(std::time::Duration::from_secs(3)).send().await {
            Ok(r) if r.status().is_success() => {
                local.reachable = true;
                if let Ok(v) = r.json::<serde_json::Value>().await {
                    local.installed_models = v.get("models").and_then(|m| m.as_array()).map(|a| a.iter().filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(String::from)).collect()).unwrap_or_default();
                }
                local.model_installed = local.installed_models.iter().any(|m| *m == config.local_model || m.strip_suffix(":latest") == Some(config.local_model.as_str()));
            }
            Ok(r) => local.error = Some(format!("Ollama answered {}", r.status())),
            Err(e) => local.error = Some(if e.is_connect() { "Ollama is not running at that address".into() } else { e.to_string() }),
        }
        Ok(AiStatus { config, local, cloud_key_present: self.cloud_key()?.is_some(), usage: self.store().ai_usage()? })
    }

    pub fn ai_audit(&self, limit: usize) -> Result<AuditReport> {
        let store = self.store();
        Ok(AuditReport { usage: store.ai_usage()?, calls: store.ai_calls(limit)? })
    }

    // ------------------------------------------------------------ model access

    fn local_llm(&self, cfg: &AiConfig) -> Llm {
        Llm::ollama(&cfg.ollama_url, &cfg.local_model, cfg.num_ctx)
    }

    /// A cloud client, only if the user has switched cloud use on and provided a key.
    fn cloud_llm(&self, cfg: &AiConfig, model: &str) -> Result<Llm> {
        if !cfg.cloud_enabled {
            return Err(Error::Invalid("cloud models are turned off. Turn them on in Settings → AI first.".into()));
        }
        let key = self.cloud_key()?.ok_or_else(|| Error::Invalid("no Anthropic API key is stored. Add one in Settings → AI.".into()))?;
        Ok(Llm::anthropic(&self.anthropic_url, model, &key))
    }

    /// Run one model call, serialising local ones and recording every call (size only).
    async fn run_llm(&self, llm: &Llm, task: &str, req: &JsonRequest) -> Result<LlmResponse> {
        let provider = llm.provider();
        let _permit = match provider {
            Provider::Local => Some(self.llm_gate.acquire().await.map_err(|_| Error::Ai("the model queue is closed".into()))?),
            Provider::Cloud => None,
        };
        let result = llm.complete_json(req).await;
        let store = self.store();
        match &result {
            Ok(r) => store.record_ai_call(provider.as_str(), &r.model, task, r.bytes_out, r.bytes_in, r.millis, None)?,
            Err(e) => store.record_ai_call(provider.as_str(), llm.model(), task, req.system.len() + req.user.len(), 0, 0, Some(&e.to_string()))?,
        }
        result
    }

    // ------------------------------------------------------------ analysis

    fn require_enabled(cfg: &AiConfig) -> Result<()> {
        if cfg.enabled {
            Ok(())
        } else {
            Err(Error::Invalid("AI features are turned off. Turn them on in Settings → AI.".into()))
        }
    }

    /// The message with its body and calendar parts downloaded (once).
    async fn body_for_analysis(&self, key: &str, mailbox: &str, uid: u32) -> Result<MessageDetail> {
        let detail = self.fetch_message(key, mailbox, uid).await?;
        if self.store().calendar_data(detail.row.id)?.is_some() {
            return Ok(detail);
        }
        // Cached before invitations were kept: download again to get the calendar parts.
        let account = self.store().find_account(key)?;
        let password = self.password_for(&account)?;
        let client = crate::imap::connect_tls(&account.imap.host, account.imap.port).await?;
        let session = crate::imap::login(client, &account.imap.username, &password).await?;
        self.fetch_message_with_session(session, &account, mailbox, uid).await
    }

    /// Triage the message and look for events in it. Results are cached; `force` recomputes.
    /// Everything here is a proposal: nothing is sent, moved, or added without the user.
    pub async fn ai_analyze(&self, key: &str, mailbox: &str, uid: u32, force: bool) -> Result<Analysis> {
        let cfg = self.ai_config()?;
        Self::require_enabled(&cfg)?;
        let (own_address, outgoing) = {
            let store = self.store();
            let account = store.find_account(key)?;
            let mb = store.find_mailbox(&account.id, mailbox)?;
            (account.email, is_outgoing_folder(&mb.name, &mb.attributes))
        };
        let detail = self.body_for_analysis(key, mailbox, uid).await?;
        let from_me = outgoing || detail.row.header.from_addr.as_deref().is_some_and(|f| f.eq_ignore_ascii_case(&own_address));
        self.analyze_detail(&cfg, detail, force, from_me).await
    }

    pub(crate) async fn analyze_detail(&self, cfg: &AiConfig, detail: MessageDetail, force: bool, from_me: bool) -> Result<Analysis> {
        let id = detail.row.id;
        let input = mail_input(&detail);
        let mut errors = Vec::new();

        let mut fresh_triage = None;
        let stored_version = self.store().triage_version(id)?;
        if force || stored_version != Some(triage::PROMPT_VERSION) {
            let llm = self.local_llm(cfg);
            let outcome = async {
                // Messages that instruct an AI get a fixed, warning result and never reach a model.
                if let Some(guarded) = triage::guard(&input) {
                    return Ok(guarded);
                }
                let r = self.run_llm(&llm, "triage", &triage::build(&input)).await?;
                triage::parse(&r.text, &r.model)
            }
            .await;
            match outcome {
                Ok(mut t) => {
                    // Mail you wrote yourself never waits for your reply.
                    if from_me {
                        t.needs_reply = false;
                    }
                    self.store().save_triage(id, &t)?;
                    fresh_triage = Some(t);
                }
                Err(e) => errors.push(format!("triage: {e}")),
            }
        }

        if force || !self.store().ai_events_done(id)? {
            let category = fresh_triage.as_ref().map(|t| t.category).or_else(|| self.stored_category(id));
            match self.find_events(cfg, id, &input, category).await {
                Ok(()) => self.store().mark_ai_events_done(id)?,
                Err(e) => errors.push(format!("events: {e}")),
            }
        }

        let store = self.store();
        let message = store.find_message(detail.row.mailbox_id, detail.row.header.uid)?;
        let proposals = store.proposals_for_message(id)?;
        Ok(Analysis { triage: fresh_triage, message, proposals, errors })
    }

    fn stored_category(&self, message_id: i64) -> Option<Category> {
        let store = self.store();
        let mb = store.mailbox_id_of(message_id).ok()?;
        store.list_messages(mb, 1_000_000).ok()?.into_iter().find(|m| m.id == message_id)?.triage.and_then(|t| Category::parse(&t.category))
    }

    async fn find_events(&self, cfg: &AiConfig, message_id: i64, input: &MailInput, category: Option<Category>) -> Result<()> {
        // Invitations first: exact, free, and always on.
        let calendar = self.store().calendar_data(message_id)?.unwrap_or_default();
        let from_invites = if calendar.is_empty() { vec![] } else { events::from_ics(&calendar, cfg) };

        // The model only reads mail that could plausibly describe an event: some date-like text,
        // and not marketing or a receipt (shipping dates are not appointments).
        let worth_asking = !matches!(category, Some(Category::Newsletter) | Some(Category::Transaction))
            && !events::blocked_by_guard(input)
            && !date_snippets(&input.body, 1, 300).is_empty();
        let from_text = if worth_asking {
            let (llm, snippets_only) = match cfg.extract_provider {
                Provider::Local => (self.local_llm(cfg), false),
                Provider::Cloud => (self.cloud_llm(cfg, &cfg.cloud_extract_model)?, true),
            };
            let r = self.run_llm(&llm, "events", &events::build(input, cfg.tz(), snippets_only)).await?;
            events::parse_ai(&r.text, input, cfg, &r.model)?
        } else {
            vec![]
        };

        let merged = events::merge(from_invites, from_text);
        self.store().store_proposals(message_id, &["ics", "ai"], &merged)?;
        Ok(())
    }

    /// Analyse the newest messages of a folder that have not been analysed yet. Stops at the first
    /// "model unreachable" error rather than waiting out a timeout for every message.
    pub async fn ai_triage_folder(&self, key: &str, mailbox: &str, limit: usize) -> Result<BatchReport> {
        let cfg = self.ai_config()?;
        Self::require_enabled(&cfg)?;
        let rows = self.list_messages(key, mailbox, limit)?;
        let mut report = BatchReport::default();
        for row in rows {
            if row.triage.is_some() {
                report.skipped += 1;
                continue;
            }
            match self.ai_analyze(key, mailbox, row.header.uid, false).await {
                Ok(a) if a.errors.is_empty() => report.analyzed += 1,
                Ok(a) => {
                    report.failed += 1;
                    let unreachable = a.errors.iter().any(|e| e.contains("not reachable") || e.contains("too long"));
                    report.errors.extend(a.errors);
                    if unreachable {
                        report.stopped_early = true;
                        break;
                    }
                }
                Err(e) => {
                    report.failed += 1;
                    report.errors.push(e.to_string());
                }
            }
        }
        Ok(report)
    }

    // ------------------------------------------------------------ proposals

    pub fn ai_proposals(&self, key: &str, mailbox: &str, uid: u32) -> Result<Vec<EventProposal>> {
        let store = self.store();
        let account = store.find_account(key)?;
        let mb = store.find_mailbox(&account.id, mailbox)?;
        let row = store.find_message(mb.id, uid)?;
        store.proposals_for_message(row.id)
    }

    pub fn ai_pending_proposals(&self, account: Option<&str>, limit: usize) -> Result<Vec<EventProposal>> {
        let store = self.store();
        let account_id = match account {
            Some(k) => Some(store.find_account(k)?.id),
            None => None,
        };
        store.pending_proposals(account_id.as_deref(), limit)
    }

    pub fn ai_accept_proposal(&self, id: i64) -> Result<crate::calendar::Event> {
        self.store().accept_proposal(id)
    }

    pub fn ai_dismiss_proposal(&self, id: i64) -> Result<EventProposal> {
        self.store().dismiss_proposal(id)
    }

    pub fn calendar_events(&self, limit: usize) -> Result<Vec<crate::calendar::Event>> {
        self.store().list_events(None, limit)
    }

    // ------------------------------------------------------------ evaluation

    /// Score a model on the built-in (or supplied) cases. Works while the master switch is off, since
    /// it is an explicit developer action on synthetic mail; cloud still needs cloud use enabled.
    pub async fn ai_eval(&self, provider: Provider, model: Option<&str>, only: Option<&str>, cases: Option<Vec<super::eval::Case>>, guards: bool) -> Result<super::eval::EvalReport> {
        let cfg = self.ai_config()?;
        let llm = match provider {
            Provider::Local => Llm::ollama(&cfg.ollama_url, model.unwrap_or(&cfg.local_model), cfg.num_ctx),
            Provider::Cloud => self.cloud_llm(&cfg, model.unwrap_or(&cfg.cloud_extract_model))?,
        };
        let cases = cases.unwrap_or_else(super::eval::builtin_cases);
        super::eval::run(&llm, &cfg, &cases, only, guards).await
    }

    // ------------------------------------------------------------ drafting

    async fn draft_prepared(&self, req: &DraftRequest) -> Result<(AiConfig, Provider, JsonRequest)> {
        let cfg = self.ai_config()?;
        Self::require_enabled(&cfg)?;
        let detail = self.fetch_message(&req.account, &req.mailbox, req.uid).await?;
        let provider = req.provider.unwrap_or(cfg.draft_provider);
        let input = DraftInput {
            from: mail_input(&detail).from,
            subject: detail.row.header.subject.clone().unwrap_or_default(),
            original: detail.body_text.clone().unwrap_or_default(),
            instruction: req.instruction.clone(),
            existing_draft: req.existing_draft.clone(),
        };
        let jr = draft::build(&input, provider == Provider::Cloud);
        Ok((cfg, provider, jr))
    }

    /// For a cloud draft: exactly what would be sent. `None` for local drafts, which send nothing.
    pub async fn ai_draft_preview(&self, req: &DraftRequest) -> Result<Option<CloudPreview>> {
        let (cfg, provider, jr) = self.draft_prepared(req).await?;
        if provider == Provider::Local {
            return Ok(None);
        }
        // Fail early if cloud cannot be used at all, before showing a preview the user cannot act on.
        self.cloud_llm(&cfg, &cfg.cloud_draft_model)?;
        Ok(Some(draft::preview(&jr, &cfg.cloud_draft_model)))
    }

    pub async fn ai_draft(&self, req: DraftRequest) -> Result<DraftResult> {
        let (cfg, provider, jr) = self.draft_prepared(&req).await?;
        let llm = match provider {
            Provider::Local => self.local_llm(&cfg),
            Provider::Cloud => {
                let llm = self.cloud_llm(&cfg, &cfg.cloud_draft_model)?;
                let preview = draft::preview(&jr, &cfg.cloud_draft_model);
                if req.confirm_hash.as_deref() != Some(preview.hash.as_str()) {
                    return Err(Error::Invalid("the cloud draft was not confirmed, or the message changed since you reviewed what would be sent. Review it again.".into()));
                }
                llm
            }
        };
        let r = self.run_llm(&llm, "draft", &jr).await?;
        Ok(DraftResult { draft: draft::parse(&r.text)?, provider, model: r.model, millis: r.millis })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sent_and_drafts_folders_hold_your_own_mail() {
        for (name, attrs, want) in [
            ("Sent Messages", "Sent", true),
            ("Sent Items", "", true),
            ("[Gmail]/Sent Mail", "Extension(\"\\\\HasNoChildren\") Sent", true),
            ("Drafts", "", true),
            ("[Gmail]/Drafts", "Drafts", true),
            ("INBOX", "NoInferiors", false),
            ("Archive", "", false),
            ("Sentinel reports", "", false),
            ("Resent", "", false),
        ] {
            assert_eq!(is_outgoing_folder(name, attrs), want, "{name}");
        }
    }
}
