//! The AI layer: triage, event extraction and reply drafting.
//!
//! Principles, all enforced in code rather than hoped for:
//! * **Local first.** A local Ollama model reads message bodies. A cloud model is opt-in per task,
//!   off by default, and sees only what the user has approved (see `draft::CloudPreview`).
//! * **Mail is hostile input.** Model output is validated against fixed enums, ranges and
//!   grounding checks; nothing the model says can cause an action. Every result is a proposal.
//! * **Auditable.** Every call is recorded by size, never by content.

pub mod draft;
pub mod eval;
pub mod events;
pub mod ics;
pub mod llm;
pub mod pipeline;
pub mod prompt;
pub mod triage;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Local,
    Cloud,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Local => "local",
            Provider::Cloud => "cloud",
        }
    }
}

/// User-editable settings, stored as JSON in the `settings` table. Secrets (the Anthropic API key)
/// live in the secret store instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AiConfig {
    /// Master switch. Off until the user turns it on; nothing is sent to any model while off.
    pub enabled: bool,
    pub ollama_url: String,
    pub local_model: String,
    pub num_ctx: u32,
    /// Which model reads messages to find events. Cloud sees only date-bearing sentences.
    pub extract_provider: Provider,
    /// Which model writes reply drafts. Cloud requires per-use confirmation of the exact payload.
    pub draft_provider: Provider,
    /// Second master switch: no cloud call is ever made while this is false.
    pub cloud_enabled: bool,
    pub cloud_extract_model: String,
    pub cloud_draft_model: String,
    /// IANA zone used to interpret times without one. Empty = the system zone.
    pub timezone: String,
    pub min_event_confidence: f32,
}

impl Default for AiConfig {
    fn default() -> Self {
        AiConfig {
            enabled: false,
            ollama_url: "http://127.0.0.1:11434".into(),
            local_model: "qwen3:8b".into(),
            num_ctx: 8192,
            extract_provider: Provider::Local,
            draft_provider: Provider::Local,
            cloud_enabled: false,
            cloud_extract_model: "claude-haiku-5-5".into(),
            cloud_draft_model: "claude-sonnet-5-5".into(),
            timezone: String::new(),
            min_event_confidence: 0.5,
        }
    }
}

impl AiConfig {
    /// The zone for naive times: the configured one, else the system's, else UTC.
    pub fn tz(&self) -> chrono_tz::Tz {
        self.timezone
            .parse()
            .ok()
            .or_else(|| iana_time_zone::get_timezone().ok().and_then(|n| n.parse().ok()))
            .unwrap_or(chrono_tz::UTC)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(self.ollama_url.starts_with("http://") || self.ollama_url.starts_with("https://")) {
            return Err("the Ollama address must start with http:// or https://".into());
        }
        if self.local_model.trim().is_empty() {
            return Err("choose a local model".into());
        }
        if !(1024..=131_072).contains(&self.num_ctx) {
            return Err("context size must be between 1024 and 131072 tokens".into());
        }
        if !self.timezone.is_empty() && self.timezone.parse::<chrono_tz::Tz>().is_err() {
            return Err(format!("'{}' is not a known time zone (use names like America/Chicago)", self.timezone));
        }
        if !(0.0..=1.0).contains(&self.min_event_confidence) {
            return Err("minimum confidence must be between 0 and 1".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_private_and_off() {
        let c = AiConfig::default();
        assert!(!c.enabled && !c.cloud_enabled);
        assert_eq!((c.extract_provider, c.draft_provider), (Provider::Local, Provider::Local));
        assert!(c.validate().is_ok());
    }

    #[test]
    fn old_or_partial_json_still_loads() {
        let c: AiConfig = serde_json::from_str(r#"{"enabled":true,"local_model":"qwen3:4b"}"#).unwrap();
        assert!(c.enabled);
        assert_eq!(c.local_model, "qwen3:4b");
        assert_eq!(c.ollama_url, AiConfig::default().ollama_url);
    }

    #[test]
    fn validation_catches_bad_settings() {
        let bad = |f: fn(&mut AiConfig)| {
            let mut c = AiConfig::default();
            f(&mut c);
            c.validate().is_err()
        };
        assert!(bad(|c| c.ollama_url = "localhost:11434".into()));
        assert!(bad(|c| c.local_model = "  ".into()));
        assert!(bad(|c| c.num_ctx = 10));
        assert!(bad(|c| c.timezone = "Mars/Olympus".into()));
        assert!(bad(|c| c.min_event_confidence = 1.5));
        let mut ok = AiConfig::default();
        ok.timezone = "America/Chicago".into();
        assert!(ok.validate().is_ok());
        assert_eq!(ok.tz(), chrono_tz::America::Chicago);
    }
}
