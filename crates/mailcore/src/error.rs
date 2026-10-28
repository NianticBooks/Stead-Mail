use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("imap error: {0}")]
    Imap(#[from] async_imap::error::Error),
    #[error("tls error: {0}")]
    Tls(String),
    /// The server rejected the request or credentials. Retrying the same thing will not help.
    #[error("smtp error: {0}")]
    Smtp(String),
    /// Network trouble or a 4xx reply. Worth retrying later.
    #[error("smtp error (temporary): {0}")]
    SmtpTransient(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("oauth error: {0}")]
    OAuth(String),
    #[error("caldav error: {0}")]
    CalDav(String),
    #[error("secret store error: {0}")]
    Secrets(String),
    #[error("account not found: {0}")]
    AccountNotFound(String),
    #[error("mailbox not found: {0}")]
    MailboxNotFound(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e.to_string())
    }
}

impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Error::Other(e.to_string())
    }
}
