//! Newline-delimited JSON-RPC 2.0 over stdio. This is the daemon boundary that the CLI,
//! external agents (Hermes, scripts) and eventually the native shells use.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::api::{AddAccountRequest, Api};
use crate::error::Error;

#[derive(Debug, Deserialize)]
pub struct Request {
    pub jsonrpc: Option<String>,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

fn ok(id: Value, result: Value) -> Response {
    Response { jsonrpc: "2.0", id, result: Some(result), error: None }
}
fn err(id: Value, code: i64, message: impl Into<String>) -> Response {
    Response { jsonrpc: "2.0", id, result: None, error: Some(RpcError { code, message: message.into() }) }
}

fn code_for(e: &Error) -> i64 {
    match e {
        Error::Invalid(_) => -32602,
        Error::AccountNotFound(_) | Error::MailboxNotFound(_) => -32001,
        Error::Secrets(_) => -32002,
        Error::Imap(_) | Error::Tls(_) | Error::Io(_) => -32003,
        _ => -32000,
    }
}

#[derive(Deserialize)]
struct AccountParams {
    account: String,
    #[serde(default = "default_mailbox")]
    mailbox: String,
    #[serde(default = "default_limit")]
    limit: u32,
}
fn default_mailbox() -> String {
    "INBOX".into()
}
fn default_limit() -> u32 {
    50
}

pub async fn dispatch(api: &Api, req: Request) -> Response {
    let id = req.id.clone().unwrap_or(Value::Null);
    let r: crate::error::Result<Value> = async {
        let to_json = |v: &dyn erased::Ser| v.to_value();
        match req.method.as_str() {
            "ping" => to_json(&api.ping()?),
            "accounts.list" => to_json(&api.list_accounts()?),
            "accounts.add" => {
                let p: AddAccountRequest = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.add_account(p)?)
            }
            "accounts.remove" => {
                let p: AccountParams = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.remove_account(&p.account)?)
            }
            "accounts.test" => {
                let p: AccountParams = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.test_connection(&p.account).await?)
            }
            "mailboxes.list" => {
                let p: AccountParams = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.list_mailboxes(&p.account)?)
            }
            "sync" => {
                let p: AccountParams = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.sync_mailbox(&p.account, &p.mailbox, p.limit).await?)
            }
            "messages.list" => {
                let p: AccountParams = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.list_messages(&p.account, &p.mailbox, p.limit as usize)?)
            }
            other => Err(Error::Invalid(format!("unknown method '{other}'"))),
        }
    }
    .await;
    match r {
        Ok(v) => ok(id, v),
        Err(e) => err(id, code_for(&e), e.to_string()),
    }
}

/// Tiny object-safe serialisation helper so `dispatch` can stay a flat match.
mod erased {
    use super::*;
    pub trait Ser {
        fn to_value(&self) -> crate::error::Result<Value>;
    }
    impl<T: Serialize> Ser for T {
        fn to_value(&self) -> crate::error::Result<Value> {
            serde_json::to_value(self).map_err(|e| Error::Other(e.to_string()))
        }
    }
}

/// Serve requests from stdin, one JSON object per line, until EOF.
pub async fn serve_stdio(api: &Api) -> std::io::Result<()> {
    let stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut lines = stdin.lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => dispatch(api, req).await,
            Err(e) => err(Value::Null, -32700, format!("parse error: {e}")),
        };
        let mut out = serde_json::to_vec(&resp).unwrap_or_else(|_| json!({"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"serialise"}}).to_string().into_bytes());
        out.push(b'\n');
        stdout.write_all(&out).await?;
        stdout.flush().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::EncryptedFileStore;
    use crate::store::Store;

    #[tokio::test]
    async fn ping_and_unknown_method() {
        let dir = std::env::temp_dir().join(format!("mailcore-rpc-{}", uuid::Uuid::new_v4()));
        let secrets = EncryptedFileStore::open_with_passphrase(&dir.join("s.json"), "pw").unwrap();
        let api = Api::new(Store::open_in_memory().unwrap(), Box::new(secrets), dir.clone());

        let req: Request = serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();
        let resp = dispatch(&api, req).await;
        assert_eq!(resp.id, json!(1));
        assert_eq!(resp.result.unwrap()["name"], "mailcore");

        let req: Request = serde_json::from_str(r#"{"jsonrpc":"2.0","id":"x","method":"nope"}"#).unwrap();
        let resp = dispatch(&api, req).await;
        assert_eq!(resp.error.unwrap().code, -32602);

        let req: Request = serde_json::from_str(r#"{"id":2,"method":"mailboxes.list","params":{"account":"ghost"}}"#).unwrap();
        let resp = dispatch(&api, req).await;
        assert_eq!(resp.error.unwrap().code, -32001);
        let _ = std::fs::remove_dir_all(dir);
    }
}
