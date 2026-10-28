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
        /// Messages to pull on the first sync of a mailbox. Later syncs fetch only changes.
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Also pull this many older messages below the oldest cached one.
        #[arg(long, default_value_t = 0)]
        backfill: u32,
    },
    /// Download and show one message (the body is cached locally after the first time).
    Show {
        account: String,
        uid: u32,
        #[arg(long, default_value = "INBOX")]
        mailbox: String,
        /// Print the raw HTML part instead of the plain text.
        #[arg(long)]
        html: bool,
    },
    /// Full-text search over cached subjects, addresses and downloaded bodies.
    Search {
        query: String,
        #[arg(long)]
        account: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Every cached message of a conversation, oldest first.
    Thread { account: String, thread_id: String },
    /// Stay connected with IDLE and re-sync whenever the server reports a change.
    Watch {
        account: String,
        #[arg(long, default_value = "INBOX")]
        mailbox: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Re-issue IDLE after this many minutes of silence (servers drop idle links at ~30).
        #[arg(long, default_value_t = 25)]
        idle_minutes: u64,
        /// Stop after this many sync passes (default: run until interrupted).
        #[arg(long)]
        cycles: Option<u32>,
    },
    /// List cached messages (newest first).
    Messages {
        account: String,
        #[arg(long, default_value = "INBOX")]
        mailbox: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Send a plain-text message through an account's SMTP server.
    Send {
        account: String,
        #[arg(long, required = true)]
        to: Vec<String>,
        #[arg(long)]
        subject: String,
        /// Message body. Use --body-stdin to read it from stdin instead.
        #[arg(long, default_value = "")]
        body: String,
        #[arg(long)]
        body_stdin: bool,
    },
    /// List the CalDAV calendars of an account (iCloud reuses the app-specific password).
    Caldav { account: String },
    /// Google Calendar login (OAuth2, Calendar scopes only).
    Google {
        #[command(subcommand)]
        cmd: GoogleCmd,
    },
    /// Run the JSON-RPC 2.0 daemon: on stdio by default, or on a Unix socket with --socket.
    Serve {
        /// Listen on the default Unix socket ($XDG_RUNTIME_DIR/mailcore.sock) instead of stdio.
        #[arg(long)]
        socket: bool,
        /// Listen on this Unix socket path instead of stdio.
        #[arg(long, conflicts_with = "socket")]
        socket_path: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand)]
enum GoogleCmd {
    /// Store the OAuth client (a 'Desktop app' client from Google Cloud Console).
    Configure {
        #[arg(long)]
        client_id: String,
        /// Read the client secret from stdin instead of the command line.
        #[arg(long)]
        client_secret_stdin: bool,
        #[arg(long)]
        client_secret: Option<String>,
    },
    /// Open the browser, sign in, and store the refresh token in the secret store.
    Login {
        /// Print the URL instead of opening a browser.
        #[arg(long)]
        no_browser: bool,
    },
    Accounts,
    /// List calendars for a signed-in Google account (email or id).
    Calendars { account: String },
    Logout { account: String },
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
                println!("IMAP login OK to {} for account '{}'; {} mailboxes:", r.imap_host, r.account, r.mailboxes.len());
                for m in &r.mailboxes {
                    println!("  {}", m.name);
                }
                match &r.smtp_error {
                    None => println!("SMTP login OK to {}", r.smtp_host),
                    Some(e) => println!("SMTP login FAILED to {}: {e}", r.smtp_host),
                }
            }),
        },
        Cmd::Mailboxes { account } => out(cli.json, &api.list_mailboxes(&account)?, |list| {
            for m in list {
                println!("{:<40} exists={:<6} uidvalidity={:<12} synced={}", m.name, fmt_opt(m.exists_count), fmt_opt(m.uidvalidity), m.last_synced_at.clone().unwrap_or_else(|| "never".into()));
            }
        }),
        Cmd::Sync { account, mailbox, limit, backfill } => {
            let opts = mailcore::sync::SyncOptions { initial_limit: limit, backfill };
            out(cli.json, &api.sync_mailbox_opts(&account, &mailbox, opts).await?, |r| println!("{}", fmt_sync(r)))
        }
        Cmd::Show { account, uid, mailbox, html } => out(cli.json, &api.fetch_message(&account, &mailbox, uid).await?, |d| {
            let h = &d.row.header;
            println!("From:    {} <{}>", h.from_name.clone().unwrap_or_default(), h.from_addr.clone().unwrap_or_default());
            println!("To:      {}", h.to_addrs.clone().unwrap_or_default());
            println!("Date:    {}", h.date.clone().or_else(|| h.internal_date.clone()).unwrap_or_default());
            println!("Subject: {}", h.subject.clone().unwrap_or_default());
            println!("Flags:   {}   Thread: {}", if h.flags.is_empty() { "(unread)" } else { &h.flags }, d.row.thread_id.clone().unwrap_or_default());
            for a in &d.attachments {
                println!("Attach:  {} ({}, {} bytes){}", a.filename.clone().unwrap_or_else(|| "(unnamed)".into()), a.content_type, a.size, if a.inline { " inline" } else { "" });
            }
            println!();
            let body = if html { d.body_html.clone() } else { d.body_text.clone() };
            println!("{}", body.unwrap_or_else(|| "(no body)".into()));
        }),
        Cmd::Search { query, account, limit } => out(cli.json, &api.search(&query, account.as_deref(), limit)?, |hits| {
            if hits.is_empty() {
                println!("no matches (search covers cached headers and downloaded bodies; sync or `show` more mail)");
            }
            for h in hits {
                let r = &h.row.header;
                println!("{:>7} {:<14.14} {:<24.24} {}\n        {}", r.uid, h.mailbox, r.from_name.clone().or_else(|| r.from_addr.clone()).unwrap_or_default(), r.subject.clone().unwrap_or_default(), h.snippet.replace('\n', " "));
            }
        }),
        Cmd::Thread { account, thread_id } => out(cli.json, &api.thread(&account, &thread_id)?, |rows| {
            for m in rows {
                let h = &m.header;
                println!("{:>7}  {:<25.25}  {:<24.24}  {}", h.uid, h.date.clone().unwrap_or_default(), h.from_name.clone().or_else(|| h.from_addr.clone()).unwrap_or_default(), h.subject.clone().unwrap_or_default());
            }
        }),
        Cmd::Watch { account, mailbox, limit, idle_minutes, cycles } => {
            eprintln!("watching {account}/{mailbox} (Ctrl-C to stop)");
            let opts = mailcore::sync::SyncOptions { initial_limit: limit, backfill: 0 };
            let json = cli.json;
            api.watch(
                &account,
                &mailbox,
                opts,
                std::time::Duration::from_secs(idle_minutes * 60),
                |r| {
                    if json {
                        println!("{}", serde_json::to_string(r).unwrap_or_default());
                    } else {
                        println!("{}  {}", chrono::Local::now().format("%H:%M:%S"), fmt_sync(r));
                    }
                },
                cycles,
            )
            .await?;
        }
        Cmd::Messages { account, mailbox, limit } => out(cli.json, &api.list_messages(&account, &mailbox, limit)?, |list| {
            for m in list {
                let h = &m.header;
                let from = h.from_name.clone().or_else(|| h.from_addr.clone()).unwrap_or_default();
                let date = h.date.clone().or_else(|| h.internal_date.clone()).unwrap_or_default();
                println!("{:>6}  {:<25.25}  {:<28.28}  {}", h.uid, date, from, h.subject.clone().unwrap_or_else(|| "(no subject)".into()));
            }
        }),
        Cmd::Send { account, to, subject, body, body_stdin } => {
            let body = if body_stdin {
                let mut b = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut b)?;
                b
            } else {
                body
            };
            let r = api.send_text(mailcore::api::SendRequest { account, to, subject, body }).await?;
            out(cli.json, &r, |r| println!("sent from {} to {} via {}", r.from, r.to.join(", "), r.smtp_host))
        }
        Cmd::Caldav { account } => out(cli.json, &api.caldav_calendars(&account).await?, |r| {
            println!("CalDAV home: {}", r.home.home);
            for c in &r.calendars {
                let kinds = match (c.supports_events, c.supports_todos) {
                    (true, true) => "events+todos",
                    (true, false) => "events",
                    (false, true) => "todos",
                    _ => "?",
                };
                println!("  {:<28} {:<13} {}", c.display_name, kinds, c.href);
            }
        }),
        Cmd::Google { cmd } => match cmd {
            GoogleCmd::Configure { client_id, client_secret_stdin, client_secret } => {
                let secret = if client_secret_stdin {
                    let mut l = String::new();
                    std::io::stdin().read_line(&mut l)?;
                    Some(l.trim().to_string())
                } else {
                    client_secret
                }
                .filter(|s| !s.is_empty());
                api.google_configure(mailcore::google::ClientConfig { client_id, client_secret: secret })?;
                println!("Google OAuth client stored in the secret store.");
            }
            GoogleCmd::Login { no_browser } => {
                let id = api
                    .google_login(|url| {
                        eprintln!("Open this URL to sign in (waiting up to 5 minutes):\n\n{url}\n");
                        if !no_browser {
                            let _ = open::that_detached(url);
                        }
                    })
                    .await?;
                out(cli.json, &id, |i| println!("signed in as {} ({} scopes); refresh token stored in the secret store", i.email, i.scopes.len()))
            }
            GoogleCmd::Accounts => out(cli.json, &api.google_identities()?, |l| {
                if l.is_empty() {
                    println!("no Google accounts. run: mailcore google login");
                }
                for i in l {
                    println!("{:<32} id {}", i.email, i.id);
                }
            }),
            GoogleCmd::Calendars { account } => out(cli.json, &api.google_calendars(&account).await?, |l| {
                for c in l {
                    println!("{:<34} {:<8} {}{}", c.summary, c.access_role, c.id, if c.primary { "  (primary)" } else { "" });
                }
            }),
            GoogleCmd::Logout { account } => out(cli.json, &api.google_logout(&account)?, |i| println!("removed {} and its stored tokens", i.email)),
        },
        Cmd::Serve { socket, socket_path } => {
            let path = socket_path.or_else(|| socket.then(mailcore::socket_path));
            match path {
                None => mailcore::rpc::serve_stdio(&api).await?,
                Some(p) => {
                    eprintln!("mailcore daemon listening on {}", p.display());
                    mailcore::rpc::serve_unix(std::sync::Arc::new(api), &p).await?
                }
            }
        }
    }
    Ok(())
}

fn fmt_sync(r: &mailcore::api::SyncReport) -> String {
    format!(
        "{}/{}: {} on server, {} cached | new {} flags {} expunged {} backfilled {}{}",
        r.account, r.mailbox, r.exists, r.cached_total, r.stored, r.flags_updated, r.expunged, r.backfilled,
        if r.uidvalidity_changed { " | UIDVALIDITY changed, cache reset" } else { "" }
    )
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
