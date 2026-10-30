//! Incremental mailbox sync, split into a network phase that touches no database and a store
//! phase that applies the result in one transaction (see `Store::apply_sync`).
//!
//! Per pass, after SELECT:
//! 1. UIDVALIDITY changed → the cache is void (RFC 3501 §2.3.1.1); start over.
//! 2. First sync → newest `initial_limit` messages by sequence number.
//! 3. Otherwise → `UID FETCH max+1:*` for new mail, one `UID FETCH min:max (FLAGS)` over the cached
//!    range for flag changes, and any cached UID missing from that reply was expunged.
//! 4. Optional backfill → the next `backfill` older messages below the lowest cached UID.

use async_imap::Session;

use crate::error::Result;
use crate::imap::{self, ImapStream, MailboxState};
use crate::store::MessageHeader;

#[derive(Debug, Clone, Copy)]
pub struct SyncOptions {
    /// How many of the newest messages to pull on the very first sync of a mailbox.
    pub initial_limit: u32,
    /// How many older messages to add below the lowest cached UID on this pass (0 = none).
    pub backfill: u32,
}

impl Default for SyncOptions {
    fn default() -> Self {
        SyncOptions { initial_limit: 50, backfill: 0 }
    }
}

/// What the store already holds for the mailbox.
#[derive(Debug, Clone, Default)]
pub struct LocalView {
    pub uidvalidity: Option<u32>,
    /// Ascending by uid.
    pub uid_flags: Vec<(u32, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct Gathered {
    pub state: MailboxState,
    /// UIDVALIDITY changed; the cached messages must be dropped before applying `new`.
    pub reset: bool,
    pub new: Vec<MessageHeader>,
    pub backfilled: usize,
    pub flag_updates: Vec<(u32, String)>,
    pub expunged: Vec<u32>,
}

pub async fn gather<T: ImapStream>(
    session: &mut Session<T>,
    mailbox: &str,
    local: LocalView,
    opts: SyncOptions,
) -> Result<Gathered> {
    let state = imap::select_state(session, mailbox).await?;
    let reset = matches!((local.uidvalidity, state.uidvalidity), (Some(old), Some(new)) if old != new);
    let cached = if reset { Vec::new() } else { local.uid_flags };
    let mut g = Gathered { state, reset, ..Default::default() };

    if state.exists == 0 {
        g.expunged = cached.iter().map(|(u, _)| *u).collect();
        return Ok(g);
    }

    let (Some(&(min, _)), Some(&(max, _))) = (cached.first(), cached.last()) else {
        g.new = imap::fetch_newest_headers(session, state.exists, opts.initial_limit).await?;
        return Ok(g);
    };

    // New mail. `n:*` always returns the highest message even when n is above it, so filter.
    g.new = imap::fetch_headers_by_uidset(session, &format!("{}:*", max.saturating_add(1)))
        .await?
        .into_iter()
        .filter(|h| h.uid > max)
        .collect();

    // Flags and expunges over the cached range in one round trip.
    let server = imap::fetch_flags_range(session, min, max).await?;
    for (uid, flags) in &cached {
        match server.get(uid) {
            None => g.expunged.push(*uid),
            Some(f) if f != flags => g.flag_updates.push((*uid, f.clone())),
            Some(_) => {}
        }
    }

    if opts.backfill > 0 && min > 1 {
        let older = imap::search_uids(session, &format!("UID 1:{}", min - 1)).await?;
        let take = opts.backfill as usize;
        let pick: Vec<String> = older.iter().rev().take(take).map(|u| u.to_string()).collect();
        if !pick.is_empty() {
            let headers = imap::fetch_headers_by_uidset(session, &pick.join(",")).await?;
            g.backfilled = headers.len();
            g.new.extend(headers);
        }
    }
    Ok(g)
}
