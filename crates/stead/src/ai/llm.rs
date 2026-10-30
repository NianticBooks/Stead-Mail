//! Provider clients. Both take the same request (system prompt, user text, JSON schema) and return
//! the model's JSON text; callers validate it. Local = Ollama's native chat API. Cloud = the
//! Anthropic Messages API with structured outputs.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct JsonRequest {
    pub system: String,
    pub user: String,
    pub schema: Value,
    pub max_tokens: u32,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub text: String,
    pub model: String,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub millis: u64,
    /// Bytes of prompt text sent and of completion text received; what the audit log records.
    pub bytes_out: usize,
    pub bytes_in: usize,
}

#[derive(Clone)]
pub struct OllamaClient {
    pub base_url: String,
    pub model: String,
    pub num_ctx: u32,
    http: reqwest::Client,
}

#[derive(Clone)]
pub struct AnthropicClient {
    pub base_url: String,
    pub model: String,
    api_key: String,
    http: reqwest::Client,
}

pub const ANTHROPIC_VERSION: &str = "2026-10-21";
pub const ANTHROPIC_URL: &str = "https://api.anthropic.com";

#[derive(Clone)]
pub enum Llm {
    Ollama(OllamaClient),
    Anthropic(AnthropicClient),
}

fn client(timeout: Duration) -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder().timeout(timeout).user_agent(concat!("stead/", env!("CARGO_PKG_VERSION"))).build().expect("http client")
}

impl Llm {
    pub fn ollama(base_url: &str, model: &str, num_ctx: u32) -> Llm {
        // A cold model load takes several seconds; generation itself is short for these tasks.
        Llm::Ollama(OllamaClient { base_url: base_url.trim_end_matches('/').into(), model: model.into(), num_ctx, http: client(Duration::from_secs(180)) })
    }

    pub fn anthropic(base_url: &str, model: &str, api_key: &str) -> Llm {
        Llm::Anthropic(AnthropicClient { base_url: base_url.trim_end_matches('/').into(), model: model.into(), api_key: api_key.into(), http: client(Duration::from_secs(90)) })
    }

    pub fn provider(&self) -> super::Provider {
        match self {
            Llm::Ollama(_) => super::Provider::Local,
            Llm::Anthropic(_) => super::Provider::Cloud,
        }
    }

    pub fn model(&self) -> &str {
        match self {
            Llm::Ollama(c) => &c.model,
            Llm::Anthropic(c) => &c.model,
        }
    }

    pub async fn complete_json(&self, req: &JsonRequest) -> Result<LlmResponse> {
        let started = Instant::now();
        let mut r = match self {
            Llm::Ollama(c) => c.complete(req).await?,
            Llm::Anthropic(c) => c.complete(req).await?,
        };
        r.millis = started.elapsed().as_millis() as u64;
        // Debug level only: this prints message-derived text, so it is off unless asked for.
        tracing::debug!(provider = self.provider().as_str(), model = %r.model, answer = %r.text, "model answer");
        r.bytes_out = req.system.len() + req.user.len();
        r.bytes_in = r.text.len();
        Ok(r)
    }
}

fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.pointer("/error/message").or_else(|| v.get("error")).and_then(|e| e.as_str().map(String::from)))
        .unwrap_or_else(|| body.chars().take(200).collect())
}

impl OllamaClient {
    async fn complete(&self, req: &JsonRequest) -> Result<LlmResponse> {
        let body = json!({
            "model": self.model,
            "stream": false,
            // qwen3's hybrid build honours this on the native API; the OpenAI-compatible endpoint does not.
            "think": false,
            "keep_alive": "10m",
            "format": req.schema,
            "options": { "temperature": 0, "num_ctx": self.num_ctx, "num_predict": req.max_tokens },
            "messages": [
                { "role": "system", "content": req.system },
                { "role": "user", "content": req.user },
            ],
        });
        let resp = self.http.post(format!("{}/api/chat", self.base_url)).json(&body).send().await.map_err(|e| {
            if e.is_connect() {
                Error::Ai(format!("Ollama is not reachable at {}. Is the service running?", self.base_url))
            } else if e.is_timeout() {
                Error::Ai("Ollama took too long to answer (is the model loading, or the GPU busy?)".into())
            } else {
                Error::Ai(format!("Ollama request failed: {e}"))
            }
        })?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| Error::Ai(e.to_string()))?;
        if !status.is_success() {
            let msg = error_message(&text);
            return Err(Error::Ai(if status.as_u16() == 404 && msg.contains("not found") {
                format!("the model '{}' is not installed. Run: ollama pull {}", self.model, self.model)
            } else {
                format!("Ollama returned {status}: {msg}")
            }));
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| Error::Ai(format!("unreadable Ollama response: {e}")))?;
        if v.get("done_reason").and_then(|d| d.as_str()) == Some("length") {
            return Err(Error::Ai("the model's answer was cut off; try a shorter message".into()));
        }
        let content = v.pointer("/message/content").and_then(|c| c.as_str()).unwrap_or("").to_string();
        if content.trim().is_empty() {
            return Err(Error::Ai("the model returned an empty answer".into()));
        }
        Ok(LlmResponse {
            text: content,
            model: self.model.clone(),
            input_tokens: v.get("prompt_eval_count").and_then(|n| n.as_u64()).map(|n| n as u32),
            output_tokens: v.get("eval_count").and_then(|n| n.as_u64()).map(|n| n as u32),
            millis: 0,
            bytes_out: 0,
            bytes_in: 0,
        })
    }
}

impl AnthropicClient {
    async fn complete(&self, req: &JsonRequest) -> Result<LlmResponse> {
        // Notes on the request shape, from the current API: no temperature/top_p/top_k (removed in
        // this API version); no forced tool choice (rejected by current models), so the JSON comes
        // from structured outputs; effort is low because these are short, well-specified tasks.
        let body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens.max(256),
            "system": req.system,
            "messages": [ { "role": "user", "content": req.user } ],
            "output_config": {
                "effort": "low",
                "format": { "type": "json_schema", "schema": req.schema },
            },
        });
        let resp = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Ai(if e.is_timeout() { "the cloud model took too long to answer".into() } else { format!("could not reach the cloud model: {e}") }))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| Error::Ai(e.to_string()))?;
        if !status.is_success() {
            let msg = error_message(&text);
            return Err(Error::Ai(match status.as_u16() {
                401 | 403 => "the Anthropic API key was rejected. Check it in Settings.".to_string(),
                429 => format!("the cloud model is rate limited right now: {msg}"),
                code => format!("the cloud model returned {code}: {msg}"),
            }));
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| Error::Ai(format!("unreadable cloud response: {e}")))?;
        match v.get("stop_reason").and_then(|s| s.as_str()) {
            Some("refusal") => return Err(Error::Ai("the cloud model declined this request".into())),
            Some("max_tokens") => return Err(Error::Ai("the cloud model's answer was cut off".into())),
            _ => {}
        }
        let content = v
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|blocks| blocks.iter().find(|b| b.get("type").and_then(|t| t.as_str()) == Some("text")))
            .and_then(|b| b.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        if content.trim().is_empty() {
            return Err(Error::Ai("the cloud model returned no text".into()));
        }
        Ok(LlmResponse {
            text: content,
            model: v.get("model").and_then(|m| m.as_str()).unwrap_or(&self.model).to_string(),
            input_tokens: v.pointer("/usage/input_tokens").and_then(|n| n.as_u64()).map(|n| n as u32),
            output_tokens: v.pointer("/usage/output_tokens").and_then(|n| n.as_u64()).map(|n| n as u32),
            millis: 0,
            bytes_out: 0,
            bytes_in: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::fake_http;

    fn req() -> JsonRequest {
        JsonRequest {
            system: "sys".into(),
            user: "usr".into(),
            schema: json!({"type":"object","properties":{"a":{"type":"string"}},"required":["a"],"additionalProperties":false}),
            max_tokens: 100,
        }
    }

    #[tokio::test]
    async fn ollama_request_uses_native_api_with_thinking_off_and_the_schema() {
        let (port, seen) = fake_http::start(200, r#"{"message":{"role":"assistant","content":"{\"a\":\"x\"}"},"done":true,"prompt_eval_count":12,"eval_count":5}"#, 1).await;
        let llm = Llm::ollama(&format!("http://127.0.0.1:{port}/"), "qwen3:8b", 8192);
        let r = llm.complete_json(&req()).await.unwrap();
        assert_eq!((r.text.as_str(), r.input_tokens, r.output_tokens), ("{\"a\":\"x\"}", Some(12), Some(5)));
        assert_eq!((r.bytes_out, r.bytes_in), (6, 9));
        assert_eq!(llm.provider(), crate::ai::Provider::Local);
        let (target, body) = seen.lock().unwrap().requests[0].clone();
        assert_eq!(target, "/api/chat");
        let b: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(b["think"], false);
        assert_eq!(b["stream"], false);
        assert_eq!(b["options"]["temperature"], 0);
        assert_eq!(b["options"]["num_ctx"], 8192);
        assert_eq!(b["format"]["required"][0], "a");
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["content"], "usr");
    }

    #[tokio::test]
    async fn ollama_errors_are_actionable() {
        let (p, _) = fake_http::start(404, r#"{"error":"model 'qwen3:99b' not found"}"#, 1).await;
        let e = Llm::ollama(&format!("http://127.0.0.1:{p}"), "qwen3:99b", 4096).complete_json(&req()).await.unwrap_err().to_string();
        assert!(e.contains("ollama pull qwen3:99b"), "{e}");

        let (p, _) = fake_http::start(200, r#"{"message":{"content":""},"done":true}"#, 1).await;
        assert!(Llm::ollama(&format!("http://127.0.0.1:{p}"), "m", 4096).complete_json(&req()).await.unwrap_err().to_string().contains("empty"));

        let (p, _) = fake_http::start(200, r#"{"message":{"content":"{\"a\":"},"done":true,"done_reason":"length"}"#, 1).await;
        assert!(Llm::ollama(&format!("http://127.0.0.1:{p}"), "m", 4096).complete_json(&req()).await.unwrap_err().to_string().contains("cut off"));

        // Nothing listening: say so plainly.
        let e = Llm::ollama("http://127.0.0.1:1", "m", 4096).complete_json(&req()).await.unwrap_err().to_string();
        assert!(e.contains("not reachable"), "{e}");
    }

    #[tokio::test]
    async fn anthropic_request_matches_the_current_api() {
        let (port, seen) = fake_http::start(
            200,
            r#"{"id":"msg_1","model":"claude-haiku-5-5","stop_reason":"end_turn","content":[{"type":"thinking","thinking":""},{"type":"text","text":"{\"a\":\"y\"}"}],"usage":{"input_tokens":30,"output_tokens":8}}"#,
            1,
        )
        .await;
        let llm = Llm::anthropic(&format!("http://127.0.0.1:{port}"), "claude-haiku-5-5", "sk-ant-test");
        let r = llm.complete_json(&req()).await.unwrap();
        assert_eq!((r.text.as_str(), r.input_tokens, r.output_tokens), ("{\"a\":\"y\"}", Some(30), Some(8)));
        assert_eq!(llm.provider(), crate::ai::Provider::Cloud);
        let seen = seen.lock().unwrap();
        let head = seen.heads[0].to_ascii_lowercase();
        assert!(head.contains("x-api-key: sk-ant-test"), "{head}");
        assert!(head.contains(&format!("anthropic-version: {}", ANTHROPIC_VERSION)), "{head}");
        assert_eq!(seen.requests[0].0, "/v1/messages");
        let b: Value = serde_json::from_str(&seen.requests[0].1).unwrap();
        assert_eq!(b["model"], "claude-haiku-5-5");
        assert_eq!(b["output_config"]["format"]["type"], "json_schema");
        assert_eq!(b["output_config"]["effort"], "low");
        assert_eq!(b["system"], "sys");
        for removed in ["temperature", "top_p", "top_k", "tool_choice", "thinking"] {
            assert!(b.get(removed).is_none(), "{removed} must not be sent");
        }
        assert!(b["max_tokens"].as_u64().unwrap() >= 256);
    }

    #[tokio::test]
    async fn anthropic_failures_map_to_clear_messages() {
        let run = |status: u16, body: &'static str| async move {
            let (p, _) = fake_http::start(status, body, 1).await;
            Llm::anthropic(&format!("http://127.0.0.1:{p}"), "claude-haiku-5-5", "k").complete_json(&req()).await.unwrap_err().to_string()
        };
        assert!(run(401, r#"{"error":{"type":"authentication_error","message":"invalid x-api-key"}}"#).await.contains("API key was rejected"));
        assert!(run(429, r#"{"error":{"message":"slow down"}}"#).await.contains("rate limited"));
        assert!(run(500, r#"{"error":{"message":"oops"}}"#).await.contains("500"));
        assert!(run(200, r#"{"stop_reason":"refusal","content":[]}"#).await.contains("declined"));
        assert!(run(200, r#"{"stop_reason":"max_tokens","content":[{"type":"text","text":"{"}]}"#).await.contains("cut off"));
        assert!(run(200, r#"{"stop_reason":"end_turn","content":[{"type":"thinking","thinking":"x"}]}"#).await.contains("no text"));
    }
}
