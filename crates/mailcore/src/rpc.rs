//! Newline-delimited JSON-RPC 2.0 over stdio. This is the daemon boundary that the CLI,
//! external agents (Hermes, scripts) and eventually the native shells use.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::api::{AddAccountRequest, Api, SendRequest};
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
struct MessageParams {
    account: String,
    #[serde(default = "default_mailbox")]
    mailbox: String,
    uid: u32,
}

#[derive(Deserialize)]
struct OutboxListParams {
    account: Option<String>,
    #[serde(default)]
    all: bool,
    #[serde(default = "default_limit")]
    limit: u32,
}

#[derive(Deserialize)]
struct OutboxIdParams {
    id: String,
}

#[derive(Deserialize)]
struct AttachmentParams {
    account: String,
    #[serde(default = "default_mailbox")]
    mailbox: String,
    uid: u32,
    index: i64,
    dir: String,
}

#[derive(Deserialize)]
struct SearchParams {
    query: String,
    account: Option<String>,
    #[serde(default = "default_limit")]
    limit: u32,
}

#[derive(Deserialize)]
struct ThreadParams {
    account: String,
    thread_id: String,
}

#[derive(Deserialize)]
struct SyncParams {
    account: String,
    #[serde(default = "default_mailbox")]
    mailbox: String,
    #[serde(default = "default_limit")]
    limit: u32,
    #[serde(default)]
    backfill: u32,
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
                let p: SyncParams = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                let opts = crate::sync::SyncOptions { initial_limit: p.limit, backfill: p.backfill };
                to_json(&api.sync_mailbox_opts(&p.account, &p.mailbox, opts).await?)
            }
            "sync.all" => {
                let p: SyncParams = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.sync_all(&p.account, crate::sync::SyncOptions { initial_limit: p.limit, backfill: p.backfill }).await?)
            }
            "outbox.list" => {
                let p: OutboxListParams = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.outbox_list(p.account.as_deref(), p.all, p.limit as usize)?)
            }
            "outbox.flush" => to_json(&api.outbox_flush().await?),
            "outbox.retry" => {
                let p: OutboxIdParams = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.outbox_retry(&p.id)?)
            }
            "outbox.cancel" => {
                let p: OutboxIdParams = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.outbox_cancel(&p.id)?)
            }
            "attachment.save" => {
                let p: AttachmentParams = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.save_attachment(&p.account, &p.mailbox, p.uid, p.index, std::path::Path::new(&p.dir)).await?)
            }
            "message.get" => {
                let p: MessageParams = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.fetch_message(&p.account, &p.mailbox, p.uid).await?)
            }
            "search" => {
                let p: SearchParams = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.search(&p.query, p.account.as_deref(), p.limit as usize)?)
            }
            "thread" => {
                let p: ThreadParams = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.thread(&p.account, &p.thread_id)?)
            }
            "send" => {
                let p: SendRequest = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.send(p).await?)
            }
            "caldav.calendars" => {
                let p: AccountParams = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.caldav_calendars(&p.account).await?)
            }
            "google.identities" => to_json(&api.google_identities()?),
            "google.calendars" => {
                let p: AccountParams = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.google_calendars(&p.account).await?)
            }
            "google.logout" => {
                let p: AccountParams = serde_json::from_value(req.params).map_err(|e| Error::Invalid(e.to_string()))?;
                to_json(&api.google_logout(&p.account)?)
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

/// Serve newline-delimited requests from any reader to any writer until EOF.
pub async fn serve_io<R, W>(api: &Api, reader: R, mut writer: W) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => dispatch(api, req).await,
            Err(e) => err(Value::Null, -32700, format!("parse error: {e}")),
        };
        let mut out = serde_json::to_vec(&resp)
            .unwrap_or_else(|_| br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"serialise"}}"#.to_vec());
        out.push(b'\n');
        writer.write_all(&out).await?;
        writer.flush().await?;
    }
    Ok(())
}

pub async fn serve_stdio(api: &Api) -> std::io::Result<()> {
    serve_io(api, tokio::io::stdin(), tokio::io::stdout()).await
}

/// Listen on a Unix socket (mode 0600, so only this user can talk to the daemon). A stale socket
/// file is removed only if nothing answers on it; a live daemon is never displaced.
#[cfg(unix)]
pub async fn serve_unix(api: std::sync::Arc<Api>, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = path.as_ref();
    if path.exists() {
        if tokio::net::UnixStream::connect(path).await.is_ok() {
            return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, format!("a daemon is already listening on {}", path.display())));
        }
        std::fs::remove_file(path)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    loop {
        let (stream, _) = listener.accept().await?;
        let api = api.clone();
        tokio::spawn(async move {
            let (r, w) = stream.into_split();
            if let Err(e) = serve_io(&api, r, w).await {
                tracing::debug!("rpc connection ended: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_socket_serves_concurrent_clients_and_is_private() {
        use std::os::unix::fs::PermissionsExt;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let dir = std::env::temp_dir().join(format!("mailcore-sock-{}", uuid::Uuid::new_v4()));
        let secrets = EncryptedFileStore::open_with_passphrase(&dir.join("s.json"), "pw").unwrap();
        let api = std::sync::Arc::new(Api::new(Store::open_in_memory().unwrap(), Box::new(secrets), dir.clone()));
        let sock = dir.join("m.sock");
        let server = tokio::spawn(serve_unix(api.clone(), sock.clone()));
        for _ in 0..50 {
            if sock.exists() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777, 0o600);

        let mut tasks = Vec::new();
        for i in 0..3 {
            let sock = sock.clone();
            tasks.push(tokio::spawn(async move {
                let mut c = tokio::net::UnixStream::connect(&sock).await.unwrap();
                c.write_all(format!("{{\"jsonrpc\":\"2.0\",\"id\":{i},\"method\":\"ping\"}}\n").as_bytes()).await.unwrap();
                let mut line = String::new();
                BufReader::new(&mut c).read_line(&mut line).await.unwrap();
                let v: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(v["id"], i);
                assert_eq!(v["result"]["name"], "mailcore");
            }));
        }
        for t in tasks { t.await.unwrap(); }

        // A second daemon on a live socket must refuse rather than steal it.
        let second = serve_unix(api.clone(), &sock).await.unwrap_err();
        assert_eq!(second.kind(), std::io::ErrorKind::AddrInUse);
        server.abort();
        let _ = std::fs::remove_dir_all(dir);
    }
}
