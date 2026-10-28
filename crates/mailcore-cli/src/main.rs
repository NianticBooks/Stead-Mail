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
        /// Sync every worthwhile folder (skips Trash, Junk, All Mail and unselectable containers).
        #[arg(long, conflicts_with = "mailbox")]
        all: bool,
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
    /// Queue a message and send it now. If the server is unreachable it stays queued and is retried.
    Send {
        account: String,
        #[arg(long, required = true)]
        to: Vec<String>,
        #[arg(long)]
        cc: Vec<String>,
        /// Blind copy: delivered but never written into the message.
        #[arg(long)]
        bcc: Vec<String>,
        #[arg(long)]
        subject: String,
        /// Message body. Use --body-stdin to read it from stdin instead.
        #[arg(long, default_value = "")]
        body: String,
        #[arg(long)]
        body_stdin: bool,
        /// Attach a file (repeatable). The file is read now, not when the message is delivered.
        #[arg(long)]
        attach: Vec<String>,
        /// Message-ID being answered (with or without angle brackets).
        #[arg(long)]
        in_reply_to: Option<String>,
        /// References chain of the original message, oldest first (repeatable).
        #[arg(long)]
        references: Vec<String>,
        /// Only queue the message; do not attempt delivery.
        #[arg(long)]
        queue_only: bool,
    },
    /// Inspect and run the outgoing queue.
    Outbox {
        #[command(subcommand)]
        cmd: OutboxCmd,
    },
    /// Save one attachment of a message (index as printed by `show`) without overwriting anything.
    Attachment {
        account: String,
        uid: u32,
        index: i64,
        #[arg(long, default_value = "INBOX")]
        mailbox: String,
        /// Directory to save into.
        #[arg(long, default_value = ".")]
        dir: std::path::PathBuf,
    },
    /// List the CalDAV calendars of an account (iCloud reuses the app-specific password).
    Caldav { account: String },
    /// Google Calendar login (OAuth2, Calendar scopes only).
    Google {
        #[command(subcommand)]
        cmd: GoogleCmd,
    },
    /// AI features: triage, event proposals, drafting, evaluation. Off until you enable them.
    Ai {
        #[command(subcommand)]
        cmd: AiCmd,
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
enum AiCmd {
    /// Show settings, whether Ollama and the model are reachable, and what has been sent where.
    Status,
    /// Turn AI features on (local model only until you also enable cloud use).
    Enable,
    Disable,
    /// Change settings. Only the options you pass are changed.
    Set {
        #[arg(long)]
        ollama_url: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        num_ctx: Option<u32>,
        /// IANA zone for times without one, e.g. America/Chicago.
        #[arg(long)]
        timezone: Option<String>,
        /// local | cloud
        #[arg(long)]
        extract_provider: Option<String>,
        /// local | cloud
        #[arg(long)]
        draft_provider: Option<String>,
        /// on | off. Cloud models are never used while this is off.
        #[arg(long)]
        cloud: Option<String>,
    },
    /// Store or remove the Anthropic API key (read from stdin, kept in the system keyring).
    Key {
        #[command(subcommand)]
        cmd: KeyCmd,
    },
    /// Triage one message and look for events in it.
    Analyze {
        account: String,
        uid: u32,
        #[arg(long, default_value = "INBOX")]
        mailbox: String,
        #[arg(long)]
        force: bool,
    },
    /// Analyse the newest messages of a folder that have not been analysed.
    Triage {
        account: String,
        #[arg(long, default_value = "INBOX")]
        mailbox: String,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// List event proposals waiting for a decision.
    Proposals {
        #[arg(long)]
        account: Option<String>,
    },
    /// Accept a proposal: it becomes a calendar event.
    Accept { id: i64 },
    Dismiss { id: i64 },
    /// Events you have accepted.
    Calendar,
    /// Draft a reply. Local by default. For --provider cloud it shows exactly what would be sent and
    /// only sends it when you repeat the command with --confirm <hash>.
    Draft {
        account: String,
        uid: u32,
        #[arg(long, default_value = "INBOX")]
        mailbox: String,
        #[arg(long, default_value = "")]
        instruction: String,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        confirm: Option<String>,
    },
    /// Every model call by size, and what has left this computer.
    Audit {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Score a model on the built-in evaluation cases (triage, events, prompt injection).
    Eval {
        #[arg(long, default_value = "local")]
        provider: String,
        #[arg(long)]
        model: Option<String>,
        /// triage | events | inject
        #[arg(long)]
        only: Option<String>,
        /// Use cases from this JSONL file instead of the built-in set.
        #[arg(long)]
        cases: Option<std::path::PathBuf>,
        /// Show every case, not just the failures.
        #[arg(long)]
        verbose: bool,
        /// Switch the deterministic injection guard off, to measure the model on its own.
        #[arg(long)]
        no_guards: bool,
        /// Run the holdout set (cases that prompts were never tuned against) instead.
        #[arg(long)]
        holdout: bool,
    },
}

#[derive(Subcommand)]
enum KeyCmd {
    Set,
    Clear,
}

#[derive(Subcommand)]
enum OutboxCmd {
    /// Show queued, sending and failed messages (add --all for sent and cancelled too).
    List {
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Attempt every message that is due, once.
    Flush,
    /// Keep flushing every N seconds until interrupted.
    Run {
        #[arg(long, default_value_t = 30)]
        every: u64,
    },
    /// Put a failed message back in the queue.
    Retry { id: String },
    /// Give up on a queued or failed message.
    Cancel { id: String },
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
        Cmd::Sync { account, mailbox, limit, backfill, all } => {
            let opts = mailcore::sync::SyncOptions { initial_limit: limit, backfill };
            if all {
                out(cli.json, &api.sync_all(&account, opts).await?, |r| {
                    for x in &r.reports {
                        println!("{}", fmt_sync(x));
                    }
                    if !r.skipped.is_empty() {
                        println!("skipped: {}", r.skipped.join(", "));
                    }
                    for (m, e) in &r.errors {
                        println!("FAILED {m}: {e}");
                    }
                })
            } else {
                out(cli.json, &api.sync_mailbox_opts(&account, &mailbox, opts).await?, |r| println!("{}", fmt_sync(r)))
            }
        }
        Cmd::Show { account, uid, mailbox, html } => out(cli.json, &api.fetch_message(&account, &mailbox, uid).await?, |d| {
            let h = &d.row.header;
            println!("From:    {} <{}>", h.from_name.clone().unwrap_or_default(), h.from_addr.clone().unwrap_or_default());
            println!("To:      {}", h.to_addrs.clone().unwrap_or_default());
            println!("Date:    {}", h.date.clone().or_else(|| h.internal_date.clone()).unwrap_or_default());
            println!("Subject: {}", h.subject.clone().unwrap_or_default());
            println!("Flags:   {}   Thread: {}", if h.flags.is_empty() { "(unread)" } else { &h.flags }, d.row.thread_id.clone().unwrap_or_default());
            for a in &d.attachments {
                println!("Attach:  [{}] {} ({}, {} bytes){}", a.part_index, a.filename.clone().unwrap_or_else(|| "(unnamed)".into()), a.content_type, a.size, if a.inline { " inline" } else { "" });
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
        Cmd::Send { account, to, cc, bcc, subject, body, body_stdin, attach, in_reply_to, references, queue_only } => {
            let body = if body_stdin {
                let mut b = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut b)?;
                b
            } else {
                body
            };
            let req = mailcore::api::SendRequest { account, to, cc, bcc, subject, body, in_reply_to, references, attachments: attach };
            if queue_only {
                out(cli.json, &api.outbox_enqueue(req)?, |i| println!("queued {} ({} bytes) to {}", short(&i.id), i.size, i.rcpts.join(", ")))
            } else {
                out(cli.json, &api.send(req).await?, |o| match o.item.state.as_str() {
                    "sent" => println!("sent {} to {}", short(&o.item.id), o.item.rcpts.join(", ")),
                    "queued" => println!("not delivered yet ({}); queued as {} and will retry", o.item.last_error.clone().unwrap_or_default(), short(&o.item.id)),
                    _ => println!("FAILED {}: {}", short(&o.item.id), o.item.last_error.clone().unwrap_or_default()),
                })
            }
        }
        Cmd::Outbox { cmd } => match cmd {
            OutboxCmd::List { account, all } => out(cli.json, &api.outbox_list(account.as_deref(), all, 100)?, |l| {
                if l.is_empty() {
                    println!("outbox is empty");
                }
                for i in l {
                    println!("{}  {:<9} tries {} {:<28.28} {}{}", short(&i.id), i.state, i.attempts, i.rcpts.join(","), i.subject, i.last_error.as_deref().map(|e| format!("\n          ! {e}")).unwrap_or_default());
                }
            }),
            OutboxCmd::Flush => out(cli.json, &api.outbox_flush().await?, |r| println!("attempted {}: sent {}, will retry {}, failed {}", r.attempted, r.sent, r.retrying, r.failed)),
            OutboxCmd::Run { every } => {
                eprintln!("flushing the outbox every {every}s (Ctrl-C to stop)");
                loop {
                    let r = api.outbox_flush().await?;
                    if r.attempted > 0 {
                        println!("{}  attempted {}: sent {}, will retry {}, failed {}", chrono::Local::now().format("%H:%M:%S"), r.attempted, r.sent, r.retrying, r.failed);
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(every)).await;
                }
            }
            OutboxCmd::Retry { id } => out(cli.json, &api.outbox_retry(&id)?, |i| println!("requeued {}", short(&i.id))),
            OutboxCmd::Cancel { id } => out(cli.json, &api.outbox_cancel(&id)?, |i| println!("cancelled {}", short(&i.id))),
        },
        Cmd::Attachment { account, uid, index, mailbox, dir } => out(cli.json, &api.save_attachment(&account, &mailbox, uid, index, &dir).await?, |a| println!("saved {} ({} bytes, {}) to {}", a.filename, a.bytes, a.content_type, a.path)),
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
        Cmd::Ai { cmd } => ai_command(&api, cmd, cli.json).await?,
        Cmd::Serve { socket, socket_path } => {
            let path = socket_path.or_else(|| socket.then(mailcore::socket_path));
            match path {
                None => mailcore::rpc::serve_stdio(&api).await?,
                Some(p) => {
                    eprintln!("mailcore daemon listening on {}", p.display());
                    let api = std::sync::Arc::new(api);
                    // The daemon is long-lived, so it is the natural owner of outbox retries.
                    let flusher = api.clone();
                    tokio::spawn(async move {
                        loop {
                            match flusher.outbox_flush().await {
                                Ok(r) if r.attempted > 0 => tracing::info!("outbox: sent {}, retrying {}, failed {}", r.sent, r.retrying, r.failed),
                                Ok(_) => {}
                                Err(e) => tracing::warn!("outbox flush failed: {e}"),
                            }
                            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        }
                    });
                    mailcore::rpc::serve_unix(api, &p).await?
                }
            }
        }
    }
    Ok(())
}

fn parse_provider(s: &str) -> Result<mailcore::ai::Provider> {
    match s.to_ascii_lowercase().as_str() {
        "local" => Ok(mailcore::ai::Provider::Local),
        "cloud" => Ok(mailcore::ai::Provider::Cloud),
        other => anyhow::bail!("unknown provider '{other}' (use local or cloud)"),
    }
}

fn fmt_ratio(r: mailcore::ai::eval::Ratio) -> String {
    format!("{:>5.1}%  ({}/{})", r.pct(), r.good, r.total)
}

async fn ai_command(api: &Api, cmd: AiCmd, json: bool) -> Result<()> {
    use mailcore::ai::Provider;
    match cmd {
        AiCmd::Status => okout(json, &api.ai_status().await?, |s| {
            let c = &s.config;
            println!("AI features:   {}", if c.enabled { "ON" } else { "off" });
            println!("Cloud models:  {} (key {})", if c.cloud_enabled { "ENABLED" } else { "off" }, if s.cloud_key_present { "stored" } else { "not set" });
            println!("Local model:   {} at {} -> {}", c.local_model, c.ollama_url,
                match (s.local.reachable, s.local.model_installed) {
                    (false, _) => format!("NOT REACHABLE ({})", s.local.error.clone().unwrap_or_default()),
                    (true, false) => format!("reachable, but the model is not installed (installed: {})", s.local.installed_models.join(", ")),
                    (true, true) => "ready".to_string(),
                });
            println!("Providers:     events -> {}, drafts -> {}   time zone {}", c.extract_provider.as_str(), c.draft_provider.as_str(), c.tz().name());
            if s.usage.is_empty() {
                println!("Usage (30 d):  no model calls yet");
            }
            for u in &s.usage {
                println!("Usage (30 d):  {:<5} {} calls, {} bytes sent, {} bytes received", u.provider, u.calls, u.bytes_out, u.bytes_in);
            }
        }),
        AiCmd::Enable | AiCmd::Disable => {
            let mut c = api.ai_config()?;
            c.enabled = matches!(cmd, AiCmd::Enable);
            api.ai_set_config(c.clone())?;
            println!("AI features are now {}.", if c.enabled { "on (local model only unless you enable cloud use)" } else { "off" });
            Ok(())
        }
        AiCmd::Set { ollama_url, model, num_ctx, timezone, extract_provider, draft_provider, cloud } => {
            let mut c = api.ai_config()?;
            if let Some(v) = ollama_url { c.ollama_url = v; }
            if let Some(v) = model { c.local_model = v; }
            if let Some(v) = num_ctx { c.num_ctx = v; }
            if let Some(v) = timezone { c.timezone = v; }
            if let Some(v) = extract_provider { c.extract_provider = parse_provider(&v)?; }
            if let Some(v) = draft_provider { c.draft_provider = parse_provider(&v)?; }
            if let Some(v) = cloud {
                c.cloud_enabled = match v.as_str() { "on" => true, "off" => false, _ => anyhow::bail!("--cloud takes on or off") };
            }
            let saved = api.ai_set_config(c)?;
            okout(json, &saved, |_| println!("settings saved"))
        }
        AiCmd::Key { cmd: KeyCmd::Set } => {
            let mut k = String::new();
            std::io::stdin().read_line(&mut k)?;
            api.ai_set_cloud_key(k.trim())?;
            println!("API key stored in the system keyring. Cloud models still stay off until: mailcore ai set --cloud on");
            Ok(())
        }
        AiCmd::Key { cmd: KeyCmd::Clear } => {
            api.ai_clear_cloud_key()?;
            println!("API key removed.");
            Ok(())
        }
        AiCmd::Analyze { account, uid, mailbox, force } => okout(json, &api.ai_analyze(&account, &mailbox, uid, force).await?, |a| {
            match &a.message.triage {
                Some(t) => println!("{} | reply needed: {} | urgency: {}\n{}", t.category, if t.needs_reply { "yes" } else { "no" }, t.urgency, t.summary),
                None => println!("(no triage)"),
            }
            for p in &a.proposals {
                let q = &p.proposal;
                println!("  [{}] #{} {} -> {}{}{}", p.state, p.id, q.title, q.start_utc, q.location.as_ref().map(|l| format!(" @ {l}")).unwrap_or_default(),
                    match q.source.as_str() { "ics" => "  (invitation)".to_string(), _ => format!("  (AI, confidence {:.0}%)", q.confidence.unwrap_or(0.0) * 100.0) });
            }
            for e in &a.errors {
                println!("  ! {e}");
            }
        }),
        AiCmd::Triage { account, mailbox, limit } => okout(json, &api.ai_triage_folder(&account, &mailbox, limit as usize).await?, |r| {
            println!("analysed {}, already done {}, failed {}{}", r.analyzed, r.skipped, r.failed, if r.stopped_early { " (stopped: the model is not reachable)" } else { "" });
            for e in r.errors.iter().take(5) { println!("  ! {e}"); }
        }),
        AiCmd::Proposals { account } => okout(json, &api.ai_pending_proposals(account.as_deref(), 100)?, |l| {
            if l.is_empty() { println!("no proposals waiting"); }
            for p in l {
                let q = &p.proposal;
                println!("#{:<4} {:<28.28} {}  {}", p.id, q.title, q.start_utc, if q.method.as_deref() == Some("CANCEL") { "CANCELLATION" } else if q.source == "ics" { "invitation" } else { "AI" });
            }
        }),
        AiCmd::Accept { id } => okout(json, &api.ai_accept_proposal(id)?, |e| println!("added to your calendar: {} at {}", e.title, e.start_utc)),
        AiCmd::Dismiss { id } => okout(json, &api.ai_dismiss_proposal(id)?, |p| println!("dismissed #{}", p.id)),
        AiCmd::Calendar => okout(json, &api.calendar_events(200)?, |l| {
            if l.is_empty() { println!("no accepted events yet"); }
            for e in l { println!("{}  {}{}", e.start_utc, e.title, e.location.as_ref().map(|x| format!(" @ {x}")).unwrap_or_default()); }
        }),
        AiCmd::Draft { account, uid, mailbox, instruction, provider, confirm } => {
            let provider = provider.map(|p| parse_provider(&p)).transpose()?;
            let req = mailcore::ai::pipeline::DraftRequest { account, mailbox, uid, instruction, existing_draft: String::new(), provider, confirm_hash: confirm.clone() };
            if provider == Some(Provider::Cloud) && confirm.is_none() {
                match api.ai_draft_preview(&req).await? {
                    Some(p) => {
                        println!("This would send {} bytes to {}:\n\n--- system ---\n{}\n\n--- message ---\n{}\n\n", p.bytes, p.model, p.system, p.user);
                        println!("To send exactly this, run the same command again with:\n  --confirm {}", p.hash);
                        return Ok(());
                    }
                    None => {}
                }
            }
            okout(json, &api.ai_draft(req).await?, |d| println!("{}\n\n[{} · {} · {} ms]", d.draft, d.provider.as_str(), d.model, d.millis))
        }
        AiCmd::Audit { limit } => okout(json, &api.ai_audit(limit as usize)?, |a| {
            for u in &a.usage { println!("{:<5} {} calls, {} bytes sent, {} bytes received (30 days)", u.provider, u.calls, u.bytes_out, u.bytes_in); }
            for c in &a.calls {
                println!("{}  {:<5} {:<18} {:<8} out {:>6} in {:>5} {:>5} ms {}", c.at, c.provider, c.model, c.task, c.bytes_out, c.bytes_in, c.millis, if c.ok { "" } else { "FAILED" });
            }
        }),
        AiCmd::Eval { provider, model, only, cases, verbose, no_guards, holdout } => {
            let cases = match cases {
                None if holdout => Some(mailcore::ai::eval::holdout_cases()),
                Some(path) => Some(std::fs::read_to_string(&path)?.lines().filter(|l| !l.trim().is_empty()).map(serde_json::from_str).collect::<std::result::Result<Vec<mailcore::ai::eval::Case>, _>>()?),
                None => None,
            };
            let provider = parse_provider(&provider)?;
            eprintln!("evaluating {} ... (a cold model load takes a few seconds)", model.as_deref().unwrap_or("the configured model"));
            let r = api.ai_eval(provider, model.as_deref(), only.as_deref(), cases, !no_guards).await?;
            okout(json, &r, |r| {
                println!("model {} ({})  {} cases  guards {}", r.model, r.provider, r.results.len(), if r.guards { "on" } else { "OFF" });
                println!("  triage category    {}", fmt_ratio(r.triage_category));
                println!("  needs-reply flag   {}", fmt_ratio(r.triage_needs_reply));
                println!("  event cases        {}", fmt_ratio(r.event_cases));
                println!("  event recall       {}", fmt_ratio(r.event_recall));
                println!("  event precision    {}", fmt_ratio(r.event_precision));
                println!("  injection resisted {}", fmt_ratio(r.injection_resisted));
                println!("  unusable answers   {}    latency mean {} ms, p95 {} ms", r.bad_answers, r.mean_millis, r.p95_millis);
                for c in &r.results {
                    if verbose || !c.passed {
                        println!("  {} {:<22} {:>6} ms {}", if c.passed { "ok  " } else { "FAIL" }, c.id, c.millis, c.notes.join("; "));
                    }
                }
            })
        }
    }
}

fn okout<T: serde::Serialize>(json: bool, v: &T, human: impl FnOnce(&T)) -> Result<()> {
    out(json, v, human);
    Ok(())
}

fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
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
