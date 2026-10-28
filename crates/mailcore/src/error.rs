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

impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Error::Other(e.to_string())
    }
}
