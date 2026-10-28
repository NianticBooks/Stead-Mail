use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A mail server endpoint. `starttls` false + port 993/465 means implicit TLS.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub starttls: bool,
    pub username: String,
}

/// How the account authenticates. OAuth2 is a Phase 1 concern; the variant exists so the
/// schema does not change later.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    /// App-specific password (iCloud, Gmail app password, generic IMAP).
    Password,
    /// XOAUTH2 bearer token (Gmail/Outlook) — not implemented yet.
    OAuth2,
}

/// Provider presets fill in known host/port values so the user only supplies an address.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Icloud,
    Gmail,
    Custom,
}

impl std::str::FromStr for Provider {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "icloud" => Ok(Provider::Icloud),
            "gmail" | "google" => Ok(Provider::Gmail),
            "custom" | "imap" => Ok(Provider::Custom),
            other => Err(Error::Invalid(format!("unknown provider '{other}'"))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub email: String,
    pub provider: Provider,
    pub auth: AuthKind,
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
}

impl Account {
    /// Build an account from a provider preset. For `Custom`, the caller must fill hosts.
    pub fn from_preset(name: &str, email: &str, provider: Provider) -> Result<Account> {
        let email = email.trim();
        if !email.contains('@') {
            return Err(Error::Invalid(format!("'{email}' is not an email address")));
        }
        let user = email.to_string();
        let (imap, smtp) = match provider {
            Provider::Icloud => (
                ServerConfig { host: "imap.mail.me.com".into(), port: 993, starttls: false, username: user.clone() },
                ServerConfig { host: "smtp.mail.me.com".into(), port: 587, starttls: true, username: user.clone() },
            ),
            Provider::Gmail => (
                ServerConfig { host: "imap.gmail.com".into(), port: 993, starttls: false, username: user.clone() },
                ServerConfig { host: "smtp.gmail.com".into(), port: 587, starttls: true, username: user.clone() },
            ),
            Provider::Custom => (
                ServerConfig { host: String::new(), port: 993, starttls: false, username: user.clone() },
                ServerConfig { host: String::new(), port: 587, starttls: true, username: user.clone() },
            ),
        };
        Ok(Account {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.trim().to_string(),
            email: email.to_string(),
            provider,
            auth: AuthKind::Password,
            imap,
            smtp,
        })
    }

    /// Key under which this account's mail password is stored in the secret store.
    pub fn password_secret_key(&self) -> String {
        format!("account/{}/password", self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icloud_preset_fills_apple_hosts() {
        let a = Account::from_preset("Personal", "fred@icloud.com", Provider::Icloud).unwrap();
        assert_eq!(a.imap.host, "imap.mail.me.com");
        assert_eq!(a.imap.port, 993);
        assert_eq!(a.smtp.port, 587);
        assert!(a.smtp.starttls);
        assert_eq!(a.auth, AuthKind::Password);
    }

    #[test]
    fn rejects_non_email() {
        assert!(Account::from_preset("x", "not-an-address", Provider::Gmail).is_err());
    }
}
