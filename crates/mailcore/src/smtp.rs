//! SMTP submission via mail-send (STARTTLS on 587 or implicit TLS on 465).

use std::time::Duration;

use mail_send::SmtpClientBuilder;
use mail_send::mail_builder::MessageBuilder;
use mail_send::mail_builder::headers::address::Address;

use crate::account::ServerConfig;
use crate::error::{Error, Result};

fn builder(server: &ServerConfig, password: &str) -> Result<SmtpClientBuilder<String>> {
    if server.host.is_empty() {
        return Err(Error::Invalid("smtp host is not configured".into()));
    }
    Ok(SmtpClientBuilder::new(server.host.clone(), server.port)
        .map_err(Error::Smtp)?
        .implicit_tls(!server.starttls)
        .credentials((server.username.clone(), password.to_string()))
        .timeout(Duration::from_secs(30)))
}

/// EHLO, STARTTLS (or implicit TLS), AUTH. Proves credentials without sending anything.
pub async fn verify_login(server: &ServerConfig, password: &str) -> Result<()> {
    let _client = builder(server, password)?.connect().await.map_err(|e| Error::Smtp(e.to_string()))?;
    Ok(())
}

pub struct OutgoingText {
    pub from_name: Option<String>,
    pub from_email: String,
    pub to: Vec<String>,
    pub subject: String,
    pub body: String,
}

pub async fn send_text(server: &ServerConfig, password: &str, msg: OutgoingText) -> Result<()> {
    if msg.to.is_empty() {
        return Err(Error::Invalid("no recipients".into()));
    }
    let from = match &msg.from_name {
        Some(n) => Address::new_address(Some(n.clone()), msg.from_email.clone()),
        None => Address::new_address(None::<String>, msg.from_email.clone()),
    };
    let to = Address::new_list(msg.to.iter().map(|e| Address::new_address(None::<String>, e.clone())).collect());
    let message = MessageBuilder::new().from(from).to(to).subject(msg.subject.clone()).text_body(msg.body.clone());
    let mut client = builder(server, password)?.connect().await.map_err(|e| Error::Smtp(e.to_string()))?;
    client.send(message).await.map_err(|e| Error::Smtp(e.to_string()))?;
    Ok(())
}
