//! `mailcore` — CLI for the core, and the JSON-RPC daemon (`mailcore serve`).

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use mailcore::api::AddAccountRequest;
use mailcore::{Api, Provider};

#[derive(Parser)]
#[command(name = "mailcore", version, about = "AI-native mail + calendar core")]
struct Cli {
    /// Emit machine-readable JSON instead of tables.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show core version, data directory and secret backend.
    Ping,
    /// Manage accounts.
    Account {
        #[command(subcommand)]
        cmd: AccountCmd,
    },
    /// List cached mailboxes for an account.
    Mailboxes { account: String },
    /// Connect to IMAP and pull the newest headers of a mailbox into the local database.
    Sync {
        account: String,
        #[arg(long, default_value = "INBOX")]
        mailbox: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// List cached messages (newest first).
    Messages {
        account: String,
        #[arg(long, default_value = "INBOX")]
        mailbox: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Run the JSON-RPC 2.0 daemon over stdio (one request per line).
    Serve,
}

#[derive(Subcommand)]
enum AccountCmd {
    /// Add an account. The password is read from stdin with --password-stdin, else prompted.
    Add(AddArgs),
    List,
    /// Remove an account (by id, name or email) and its stored password.
    Remove { account: String },
    /// Log in over IMAP and list mailboxes without storing anything.
    Test { account: String },
}

#[derive(Args)]
struct AddArgs {
    #[arg(long)]
    name: String,
    #[arg(long)]
    email: String,
    /// icloud | gmail | custom
    #[arg(long, default_value = "custom")]
    provider: String,
    #[arg(long)]
    imap_host: Option<String>,
    #[arg(long)]
    imap_port: Option<u16>,
    #[arg(long)]
    smtp_host: Option<String>,
    #[arg(long)]
    smtp_port: Option<u16>,
    /// Read the password from the first line of stdin instead of prompting.
    #[arg(long)]
    password_stdin: bool,
    /// Replace an existing account with the same name or email (and its stored password).
    #[arg(long)]
    replace: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("mailcore=info".parse()?))
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let api = Api::open_default().context("opening mailcore data")?;

    match cli.cmd {
        Cmd::Ping => out(cli.json, &api.ping()?, |p| {
            println!("mailcore {}  data: {}  secrets: {}  accounts: {}", p.version, p.data_dir, p.secrets_backend, p.accounts)
        }),
        Cmd::Account { cmd } => match cmd {
            AccountCmd::Add(a) => {
                let provider: Provider = a.provider.parse()?;
                let password = if a.password_stdin {
                    let mut s = String::new();
                    std::io::stdin().read_line(&mut s)?;
                    s.trim_end_matches(['\r', '\n']).to_string()
                } else {
                    rpassword::prompt_password(format!("password for {}: ", a.email))?
                };
                let acct = api.add_account(AddAccountRequest {
                    name: a.name,
                    replace: a.replace,
                    email: a.email,
                    provider,
                    password,
                    imap_host: a.imap_host,
                    imap_port: a.imap_port,
                    smtp_host: a.smtp_host,
                    smtp_port: a.smtp_port,
                })?;
                out(cli.json, &acct, |a| println!("added {} <{}>  imap {}:{}  id {}", a.name, a.email, a.imap.host, a.imap.port, a.id))
            }
            AccountCmd::List => out(cli.json, &api.list_accounts()?, |list| {
                if list.is_empty() {
                    println!("no accounts. add one with: mailcore account add --name NAME --email EMAIL --provider icloud|gmail|custom");
                }
                for a in list {
                    println!("{:<12} {:<32} {:>7}  imap {}:{}", a.name, a.email, format!("{:?}", a.provider).to_lowercase(), a.imap.host, a.imap.port);
                }
            }),
            AccountCmd::Remove { account } => out(cli.json, &api.remove_account(&account)?, |a| println!("removed {} <{}>", a.name, a.email)),
            AccountCmd::Test { account } => out(cli.json, &api.test_connection(&account).await?, |r| {
                println!("login OK to {} for account '{}'; {} mailboxes:", r.imap_host, r.account, r.mailboxes.len());
                for m in &r.mailboxes {
                    println!("  {}", m.name);
                }
            }),
        },
        Cmd::Mailboxes { account } => out(cli.json, &api.list_mailboxes(&account)?, |list| {
            for m in list {
                println!("{:<40} exists={:<6} uidvalidity={:<12} synced={}", m.name, fmt_opt(m.exists_count), fmt_opt(m.uidvalidity), m.last_synced_at.clone().unwrap_or_else(|| "never".into()));
            }
        }),
        Cmd::Sync { account, mailbox, limit } => out(cli.json, &api.sync_mailbox(&account, &mailbox, limit).await?, |r| {
            println!("synced {}/{}: {} on server, fetched {}, stored {}, {} mailboxes{}", r.account, r.mailbox, r.exists, r.fetched, r.stored, r.mailboxes_seen, if r.uidvalidity_changed { " (UIDVALIDITY changed, cache reset)" } else { "" })
        }),
        Cmd::Messages { account, mailbox, limit } => out(cli.json, &api.list_messages(&account, &mailbox, limit)?, |list| {
            for m in list {
                let h = &m.header;
                let from = h.from_name.clone().or_else(|| h.from_addr.clone()).unwrap_or_default();
                let date = h.date.clone().or_else(|| h.internal_date.clone()).unwrap_or_default();
                println!("{:>6}  {:<25.25}  {:<28.28}  {}", h.uid, date, from, h.subject.clone().unwrap_or_else(|| "(no subject)".into()));
            }
        }),
        Cmd::Serve => mailcore::rpc::serve_stdio(&api).await?,
    }
    Ok(())
}

fn out<T: serde::Serialize>(json: bool, v: &T, human: impl FnOnce(&T)) {
    if json {
        println!("{}", serde_json::to_string_pretty(v).expect("serialise"));
    } else {
        human(v);
    }
}

fn fmt_opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "-".into())
}
