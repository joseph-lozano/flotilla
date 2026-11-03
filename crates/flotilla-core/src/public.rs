//! Public preview links (`flotilla_proto::public`): a port's link with no sign-in token, for
//! an hour, a day, a week or until you make it private. Only you make one (agents are refused,
//! `Request::agent_refusal`).
//!
//! What boat does (seen live 2026-11-02, `design/public-links.md`):
//! - `POST /sandboxes/{id}/host {"public": true}` answers the same URL with no token, and
//!   anyone can open it at once. It opens the port in the VM's firewall too.
//! - `{"public": false}` after it makes a new token (the old private link stops working too),
//!   and the URL without one is refused (403) at once. The firewall rule goes.
//! - Nothing reads a port's access back (the sandbox doesn't list its ports), so what's public
//!   is what Flotilla made public, kept here.
//! - A stopped VM takes neither (`400 machine_not_running`) and serves nothing. After a resume
//!   the route served again only once the port was hosted again.
//!
//! So:
//! - Each link made public is kept in `kv` as `public_link:<thread>:<port>`: the VM, the URL,
//!   since and until when. It's kept before boat is asked, so a public link is never left
//!   untracked (an answer lost on the way, a crash): one boat may have made public that
//!   Flotilla can't confirm is put back to private, and until that's confirmed it shows as
//!   ending and is tried again.
//! - The engine's worker (`flotilla_engine::public`) looks through a [`Sweeper`], which makes
//!   each private when its time is up, whether or not an app is open, on workers of their own,
//!   so the clock never waits for boat. A restart loses nothing; a link due
//!   while the server was off ends at its first look. A rebuilt database carries them over
//!   (`Store::backup`, made whole beside the old one before the two are swapped), and a link
//!   whose thread isn't back yet is kept, and ended through boat alone when it's due.
//! - The clock runs while the VM is stopped: an hour is an hour of the day, not of the VM. One
//!   whose time runs out then (or that you make private, or whose thread you archive) is
//!   `ended`, and made private as the VM wakes ([`on_wake`]), before anything is sent to it. One
//!   with time left is hosted public again then, so it carries on working. A link is hidden
//!   only once boat has made it private or its VM is stopped: one boat failed to make private
//!   on a VM that's up stays shown, as ending, and is tried again at every look.
//! - Deleting or forgetting a thread ([`forgetting`]) makes its links due and keeps them: the
//!   worker ends them through boat alone and forgets each once boat has it private or says its
//!   VM is gone (forgetting leaves the VM up). A link on a VM its thread has moved off (a fork
//!   labelled as it, say) is ended there the same way: that VM may still serve it.
//! - Every host call for a thread goes through here, under its lock, so none (a preview
//!   opened, a service's link, the worker) makes a public link private, or a private one
//!   public, behind another's back. A preview of a link past its time hosts it private. Each
//!   also waits for a rebuild under way, and is refused through a connection opened before one.
//! - Making a link private changes its token: the newest private URL is kept
//!   (`preview_url:<thread>:<port>`) and services' links take it (`services::state`).

use crate::boat::BoatClient;
use crate::error::{Error, Result};
use crate::store::{Store, Thread, ThreadState};
use flotilla_proto::{PublicFor, PublicLink};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const PREFIX: &str = "public_link:";
const URL_PREFIX: &str = "preview_url:";
/// For testing a link's end: every timed choice lasts this many seconds instead (10 or more;
/// only ever shorter). Until-stopped is never shortened.
pub const TEST_SECS_ENV: &str = "FLOTILLA_DEBUG_PUBLIC_SECS";
/// A link boat can't be told about yet (its VM stopping or starting, or its thread not here
/// while a rebuild syncs) is tried at most this often.
const RETRY_MS: i64 = 60_000;
/// Threads whose links the worker ends at once, at most (each a boat call, up to its timeout).
const SWEEP_AT_ONCE: usize = 8;

/// Threads whose links are being changed (a host call and the record it goes with), and the
/// signal that one was let go: one at a time for each thread, and a slow call for one holds up
/// no other's.
static HELD: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
static FREED: Condvar = Condvar::new();

/// A thread held ([`hold`]): let go when dropped.
struct Held(String);

impl Drop for Held {
    fn drop(&mut self) {
        HELD.lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
        FREED.notify_all();
    }
}

fn hold(thread: &str) -> Held {
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    while held.contains(thread) {
        held = FREED.wait(held).unwrap_or_else(|e| e.into_inner());
    }
    held.insert(thread.to_string());
    Held(thread.to_string())
}

/// `hold`, waiting at most `patience`.
fn try_hold(thread: &str, patience: Duration) -> Option<Held> {
    let deadline = Instant::now() + patience;
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    while held.contains(thread) {
        let left = deadline.checked_duration_since(Instant::now())?;
        held = FREED
            .wait_timeout(held, left)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
    held.insert(thread.to_string());
    Some(Held(thread.to_string()))
}

/// The database in use: `store`, or, if a rebuild has moved its file aside since it was opened,
/// the one in its place, opened now. What's done to a link is done there.
fn current(store: &Store) -> Result<Option<Store>> {
    match store.path() {
        Some(path) if store.stale() => Ok(Some(Store::open(path)?)),
        _ => Ok(None),
    }
}

/// The thread as the database in use has it now (read with its lock held): what a link's end
/// is decided by, not the caller's copy, which may be from a database moved aside.
fn thread_now(store: &Store, id: &str) -> Option<Thread> {
    store.thread(id).ok().flatten()
}

/// For anything that hosts a port or ends a link (taken before the thread's lock): none while
/// the database is rebuilt (`Store::backup` write-holds the gate), and none through a connection
/// opened before it was (it has the file moved aside, so what it reads of a link is stale and
/// what it writes is lost): callers pass [`current`]'s.
fn changing(store: &Store) -> Result<crate::store::GateRead> {
    let gate = crate::store::gate_read(store.path());
    if store.stale() {
        return Err(Error::Invalid(
            "The database was just rebuilt: try again".into(),
        ));
    }
    Ok(gate)
}

/// A link made public, as kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub thread: String,
    pub port: u16,
    /// The VM it's public on.
    pub sandbox: String,
    /// Its URL, with no token ("" until boat has answered, if Flotilla knew none for the port).
    pub url: String,
    pub since_ms: i64,
    /// None: until you make it private.
    pub until_ms: Option<i64>,
    /// Over (its time ran out, you made it private, its thread was archived) while boat
    /// couldn't be told (the VM stopped): made private as the VM wakes. No longer shown.
    #[serde(default)]
    pub ended: bool,
    /// When boat was last asked to make it private and couldn't.
    #[serde(default)]
    pub tried_ms: Option<i64>,
}

impl Record {
    pub fn due(&self, now: i64) -> bool {
        self.until_ms.is_some_and(|u| u <= now)
    }

    fn link(&self) -> PublicLink {
        PublicLink {
            port: self.port,
            url: self.url.clone(),
            since_ms: self.since_ms,
            until_ms: self.until_ms,
        }
    }
}

fn key(thread: &str, port: u16) -> String {
    format!("{PREFIX}{thread}:{port}")
}

fn url_key(thread: &str, port: u16) -> String {
    format!("{URL_PREFIX}{thread}:{port}")
}

/// The `kv` prefixes of what's kept for a thread that goes with it: its private URLs. Its
/// links don't (see [`forgetting`]).
pub fn thread_prefixes(thread: &str) -> [String; 1] {
    [format!("{URL_PREFIX}{thread}:")]
}

/// The `kv` prefixes a rebuilt database carries over: what's public on boat must stay known.
pub const CARRIED_PREFIXES: [&str; 2] = [PREFIX, URL_PREFIX];

/// Every link kept, the thread's alone with `thread`.
pub fn records(store: &Store, thread: Option<&str>) -> Vec<Record> {
    let prefix = thread.map_or_else(|| PREFIX.to_string(), |t| format!("{PREFIX}{t}:"));
    store
        .kv_with_prefix(&prefix)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_str(&v).ok())
        .collect()
}

fn get(store: &Store, thread: &str, port: u16) -> Option<Record> {
    store
        .kv_get(&key(thread, port))
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok())
}

fn put(store: &Store, r: &Record) -> Result<()> {
    store.kv_set(
        &key(&r.thread, r.port),
        &serde_json::to_string(r).map_err(|e| Error::Invalid(e.to_string()))?,
    )
}

fn forget(store: &Store, r: &Record) -> Result<()> {
    store.kv_delete(&key(&r.thread, r.port))
}

/// When a link made public now `for` this long goes private: `TEST_SECS_ENV` shortens it.
pub fn until(for_: PublicFor, now: i64) -> Option<i64> {
    until_with(for_, now, std::env::var(TEST_SECS_ENV).ok().as_deref())
}

fn until_with(for_: PublicFor, now: i64, test_secs: Option<&str>) -> Option<i64> {
    let ms = for_.millis()?;
    let test = test_secs
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|s| *s >= 10)
        .map(|s| s * 1000);
    Some(now + test.map_or(ms, |t| t.min(ms)))
}

fn up(t: &Thread) -> bool {
    matches!(t.state, ThreadState::Idle | ThreadState::Running)
}

/// Its VM is stopped for sure: boat can't be told, and it serves nothing. (Stopping, starting
/// or in error, boat is asked: it says if it's stopped.)
fn stopped(t: &Thread) -> bool {
    t.state == ThreadState::Stopped
}

/// Its VM is gone (nothing of the link is left on boat), or the thread is on another one now
/// (a fork labelled as it, say): not the thread's link any more.
fn orphaned(r: &Record, t: &Thread) -> bool {
    t.vm_gone() || moved(r, t)
}

/// The thread is on another VM now: its link's own VM, which may still serve it, is told as if
/// its thread weren't here.
fn moved(r: &Record, t: &Thread) -> bool {
    t.sandbox_id.as_deref() != Some(r.sandbox.as_str())
}

/// The links anyone may open on the thread's VM, by port: what its page shows. One past its
/// time that boat hasn't been told about yet still shows (it's still public).
pub fn shown(store: &Store, t: &Thread) -> Vec<PublicLink> {
    let mut out: Vec<PublicLink> = records(store, Some(&t.id))
        .into_iter()
        .filter(|r| !r.ended && !orphaned(r, t))
        .map(|r| r.link())
        .collect();
    out.sort_by_key(|l| l.port);
    out
}

/// The private URL boat gave last for a port on this VM, if it's had to make a new one.
pub fn private_url(store: &Store, thread: &str, sandbox: &str, port: u16) -> Option<String> {
    #[derive(Deserialize)]
    struct Kept {
        sandbox: String,
        url: String,
    }
    let kept: Kept = store
        .kv_get(&url_key(thread, port))
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok())?;
    (kept.sandbox == sandbox).then_some(kept.url)
}

fn remember(store: &Store, thread: &str, sandbox: &str, port: u16, url: &str) {
    let _ = store.kv_set(
        &url_key(thread, port),
        &serde_json::json!({"sandbox": sandbox, "url": url}).to_string(),
    );
}

/// The URL to open a port on (Preview…, a service's link): the public one while it's public
/// (hosted public again, as boat has it), else a private one. One past its time is made
/// private here, not hosted public again.
pub fn host(
    boat: &BoatClient,
    store: &Store,
    thread: &str,
    sandbox: &str,
    port: u16,
) -> Result<String> {
    let own = current(store)?;
    let store = own.as_ref().unwrap_or(store);
    let _gate = changing(store)?;
    let _held = hold(thread);
    let now = crate::util::now_ms();
    let archived = store.is_archived(thread).unwrap_or(false);
    match get(store, thread, port).filter(|r| r.sandbox == sandbox) {
        Some(r) if !r.ended && !r.due(now) && !archived => {
            let url = boat.host_port(sandbox, port, true)?;
            if url != r.url {
                put(
                    store,
                    &Record {
                        url: url.clone(),
                        ..r
                    },
                )?;
            }
            Ok(url)
        }
        over => {
            let url = boat.host_port(sandbox, port, false)?;
            remember(store, thread, sandbox, port, &url);
            if let Some(r) = over {
                forget(store, &r)?;
                eprintln!("[public] {thread} port {port} private");
            }
            Ok(url)
        }
    }
}

/// Make the port's link public `for` this long from `now` (again, if it's public already: its
/// time starts over). The thread's VM must be up. Returns the link.
///
/// Kept before boat is asked. If boat fails, a link that was public stays as it was; one that
/// wasn't is put back to private, and if boat can't confirm that either, it shows as ending
/// and the worker tries again.
pub fn make_public(
    boat: &BoatClient,
    store: &Store,
    t: &Thread,
    port: u16,
    for_: PublicFor,
    now: i64,
) -> Result<String> {
    let sandbox = t
        .sandbox_id
        .clone()
        .ok_or_else(|| Error::Invalid("the thread has no VM".into()))?;
    let own = current(store)?;
    let store = own.as_ref().unwrap_or(store);
    let _gate = changing(store)?;
    let _held = hold(&t.id);
    // Forgotten meanwhile (`forgetting`; or a rebuild hasn't brought it back yet), or on
    // another VM: a link made now would be kept for a thread that isn't here.
    match thread_now(store, &t.id) {
        None => {
            return Err(Error::Invalid(
                "The thread isn't in the database now: try again".into(),
            ));
        }
        Some(now) if now.sandbox_id.as_deref() != Some(sandbox.as_str()) => {
            return Err(Error::Invalid(
                "The thread is on another VM now: try again".into(),
            ));
        }
        Some(_) => {}
    }
    // One record a port: its link on a VM the thread has moved off, which may still serve it,
    // is ended there first.
    if let Some(old) = get(store, &t.id, port).filter(|r| r.sandbox != sandbox)
        && !end(boat, store, None, old, now)?
    {
        return Err(Error::Invalid(format!(
            "Port {port}'s link on the thread's last VM hasn't ended yet"
        )));
    }
    let before = get(store, &t.id, port).filter(|r| r.sandbox == sandbox && !r.ended);
    let known = before
        .as_ref()
        .map(|r| r.url.clone())
        .or_else(|| private_url(store, &t.id, &sandbox, port).map(|u| crate::services::origin(&u)));
    let since_ms = before.as_ref().map_or(now, |r| r.since_ms);
    let mut r = Record {
        thread: t.id.clone(),
        port,
        sandbox: sandbox.clone(),
        url: known.unwrap_or_default(),
        since_ms,
        until_ms: until(for_, now),
        ended: false,
        tried_ms: None,
    };
    put(store, &r)?;
    match boat.host_port(&sandbox, port, true) {
        Ok(url) => {
            if url != r.url {
                r.url = url.clone();
                put(store, &r)?;
            }
            eprintln!("[public] {} port {port} public {}", t.id, for_.words());
            Ok(url)
        }
        Err(e) => {
            match before {
                Some(b) => put(store, &b)?,
                None => {
                    if let Err(again) = end(boat, store, Some(t), r, now) {
                        eprintln!(
                            "[public] {} port {port}: couldn't make it public, nor be sure it's private ({}); trying again",
                            t.id,
                            again.plain()
                        );
                    }
                }
            }
            Err(e.into())
        }
    }
}

/// Make the port's link private at once: with the VM up, boat is told now (a new token); with
/// it stopped, as it wakes (it serves nothing meanwhile). Nothing to do if it isn't public. If
/// boat fails, it shows as ending and the worker tries again.
pub fn make_private(boat: &BoatClient, store: &Store, t: &Thread, port: u16) -> Result<()> {
    let own = current(store)?;
    let store = own.as_ref().unwrap_or(store);
    let _gate = changing(store)?;
    let _held = hold(&t.id);
    match get(store, &t.id, port) {
        Some(r) => end(
            boat,
            store,
            thread_now(store, &t.id).as_ref(),
            r,
            crate::util::now_ms(),
        )
        .map(|_| ()),
        None => Ok(()),
    }
}

/// Archiving, first: every live link of the thread is due `now`, kept in its own record (a
/// rebuild carries those, not the archive), so they end however archiving is cut off; making
/// one public again is what changes that.
pub fn closing(store: &Store, thread: &str, now: i64) -> Result<()> {
    let own = current(store)?;
    let store = own.as_ref().unwrap_or(store);
    let _gate = changing(store)?;
    let _held = hold(thread);
    for mut r in records(store, Some(thread))
        .into_iter()
        .filter(|r| !r.ended && !r.due(now))
    {
        r.until_ms = Some(now);
        put(store, &r)?;
    }
    Ok(())
}

/// A thread's rows going (`rows`: `Store::delete_thread`), whether its VM was deleted or is
/// left as it is (`thread.forget`): with the thread held, every link it has is due `now` and
/// kept, for the worker to end through boat alone (as it does one whose thread a rebuild hasn't
/// brought back) and then forget, once boat has it private or says its VM is gone. None is
/// made public meanwhile ([`make_public`] refuses a thread that isn't here). `patience`: how
/// long to wait while its links are being changed (None: as long as it takes); past it,
/// nothing is done.
pub fn forgetting(
    store: &Store,
    thread: &str,
    now: i64,
    patience: Option<Duration>,
    rows: impl FnOnce(&Store) -> Result<()>,
) -> Result<()> {
    let own = current(store)?;
    let store = own.as_ref().unwrap_or(store);
    let _gate = changing(store)?;
    let _held = match patience {
        None => hold(thread),
        Some(p) => try_hold(thread, p).ok_or_else(|| {
            Error::Invalid("A link of the thread is being changed: try again".into())
        })?,
    };
    for mut r in records(store, Some(thread))
        .into_iter()
        .filter(|r| !r.ended && !r.due(now))
    {
        r.until_ms = Some(now);
        put(store, &r)?;
    }
    rows(store)
}

/// End a link (with its thread's lock held). Unless its VM is stopped for sure, boat is asked
/// to make it private (its thread not here, a rebuild not synced yet, too); with the VM stopped
/// (or boat saying so), it's `ended`, for the wake. True if boat made it private, or nothing of
/// it is left on boat. A failure leaves it shown, due now, and tried again: as far as anyone
/// knows it's public.
fn end(
    boat: &BoatClient,
    store: &Store,
    t: Option<&Thread>,
    mut r: Record,
    now: i64,
) -> Result<bool> {
    // The thread's VM now says nothing of another's.
    let t = t.filter(|t| !moved(&r, t));
    if t.is_some_and(|t| t.vm_gone()) {
        forget(store, &r)?;
        return Ok(true);
    }
    if !t.is_some_and(stopped) {
        match boat.host_port(&r.sandbox, r.port, false) {
            Ok(url) => {
                if t.is_some() {
                    remember(store, &r.thread, &r.sandbox, r.port, &url);
                }
                forget(store, &r)?;
                eprintln!("[public] {} port {} private", r.thread, r.port);
                return Ok(true);
            }
            // The VM is gone: nothing is public.
            Err(e) if t.is_none() && e.status() == Some(404) => {
                forget(store, &r)?;
                return Ok(true);
            }
            // Stopped after all: as below.
            Err(e) if e.is("machine_not_running") => {}
            Err(e) => {
                r.ended = false;
                r.until_ms = Some(r.until_ms.map_or(now, |u| u.min(now)));
                r.tried_ms = Some(now);
                put(store, &r)?;
                return Err(e.into());
            }
        }
    }
    let was = r.ended;
    r.ended = true;
    r.tried_ms = Some(now);
    put(store, &r)?;
    if !was {
        eprintln!(
            "[public] {} port {} private as its VM wakes",
            r.thread, r.port
        );
    }
    Ok(false)
}

/// Archiving: every link of the thread goes private (as its VM wakes, if boat can't be told
/// now: it's stopped right after). One boat fails on stays shown, ending, for the worker.
pub fn end_all(boat: &BoatClient, store: &Store, t: &Thread) {
    // Should this not get as far (a crash, a rebuild under way), the worker ends an archived
    // thread's links all the same (`worth`).
    let own = current(store);
    let store = match &own {
        Ok(Some(s)) => s,
        _ => store,
    };
    let Ok(_gate) = changing(store) else {
        eprintln!(
            "[public] {}: the database was rebuilt as it was archived; its links end at the next look",
            t.id
        );
        return;
    };
    let _held = hold(&t.id);
    let now = crate::util::now_ms();
    let t_now = thread_now(store, &t.id);
    for r in records(store, Some(&t.id)) {
        if let Err(e) = end(boat, store, t_now.as_ref(), r.clone(), now) {
            eprintln!(
                "[public] {} port {}: couldn't make it private as it was archived: {}",
                r.thread,
                r.port,
                e.plain()
            );
        }
    }
}

/// The thread's VM is up again (Flotilla woke it): links that ended meanwhile go private, the
/// rest are hosted public again so they carry on working.
pub fn on_wake(boat: &BoatClient, store: &Store, t: &Thread, now: i64) {
    let own = current(store);
    let store = match &own {
        Ok(Some(s)) => s,
        _ => store,
    };
    let Ok(_gate) = changing(store) else {
        eprintln!(
            "[public] {}: the database was rebuilt as its VM woke; its links are seen to at the next look",
            t.id
        );
        return;
    };
    let _held = hold(&t.id);
    // The database in use's, or (a rebuild not synced yet) the caller's, read from boat as it woke.
    let t_now = thread_now(store, &t.id);
    let t = t_now.as_ref().unwrap_or(t);
    // Woken for `.agents/archive`, say: an archived thread keeps nothing public.
    let archived = store.is_archived(&t.id).unwrap_or(false);
    for r in records(store, Some(&t.id)) {
        if r.ended || r.due(now) || archived || orphaned(&r, t) {
            if let Err(e) = end(boat, store, Some(t), r.clone(), now) {
                eprintln!(
                    "[public] {} port {}: couldn't make it private as the VM woke: {}",
                    r.thread,
                    r.port,
                    e.plain()
                );
            }
        } else if let Err(e) = boat.host_port(&r.sandbox, r.port, true) {
            eprintln!(
                "[public] {} port {}: couldn't host it again as the VM woke: {}",
                r.thread,
                r.port,
                Error::from(e).plain()
            );
        }
    }
}

/// What a sweep did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Swept {
    /// Threads whose links changed.
    pub changed: BTreeSet<String>,
    /// The next link's end, if one is timed.
    pub next_ms: Option<i64>,
    /// Some couldn't be ended (boat failed): look again soon.
    pub failed: bool,
    /// Some were left for the next look: their thread's lock was held (a preview, say).
    pub busy: bool,
}

/// Whether a sweep at `now` has anything to do for `r`, its thread as it is (and whether it's
/// archived: an archived thread keeps nothing public, however archiving got cut off).
fn worth(r: &Record, t: Option<&Thread>, archived: bool, now: i64) -> bool {
    let retry = r.tried_ms.is_none_or(|at| now - at >= RETRY_MS);
    match t {
        // Ended now on its own VM, and again a while after a failure.
        Some(t) if moved(r, t) => retry,
        Some(t) if t.vm_gone() => true,
        // Ended while stopped: once it's up, or now and then while it's neither up nor stopped.
        Some(t) if r.ended => up(t) || (!stopped(t) && retry),
        Some(_) => r.due(now) || archived,
        None => (r.ended || r.due(now)) && retry,
    }
}

/// One thread's share of a look: its ports to see to.
struct Due {
    ports: Vec<u16>,
}

/// What a look at `now` has to do, by thread (read without the threads held: each is read
/// again with its thread held), and when the next link not yet due is.
fn plan(store: &Store, now: i64) -> (BTreeMap<String, Due>, Option<i64>) {
    let mut work: BTreeMap<String, Due> = BTreeMap::new();
    let mut next_ms: Option<i64> = None;
    for r in records(store, None) {
        let t = store.thread(&r.thread).ok().flatten();
        let archived = store.is_archived(&r.thread).unwrap_or(false);
        if worth(&r, t.as_ref(), archived, now) {
            work.entry(r.thread.clone())
                .or_insert_with(|| Due { ports: Vec::new() })
                .ports
                .push(r.port);
        } else if !r.ended
            && let Some(u) = r.until_ms
        {
            next_ms = Some(next_ms.map_or(u, |n| n.min(u)));
        }
    }
    (work, next_ms)
}

/// End what's due at `now`: links whose time is up (or that boat failed to make private), and
/// those that ended while their VM was stopped and are on a VM that's up now (woken outside
/// Flotilla). Forget those whose VM is gone or replaced. A link whose thread isn't here is kept
/// (a rebuild brings it back), and ended through boat alone when it's due. One thread at a time,
/// waiting for each; a thread held long (a preview under way) is left for the next look. The
/// worker uses a [`Sweeper`], which never waits for boat.
pub fn sweep(boat: &BoatClient, store: &Store, now: i64) -> Swept {
    let (work, next_ms) = plan(store, now);
    let mut out = Swept {
        next_ms,
        ..Default::default()
    };
    for (thread, due) in &work {
        let d = sweep_thread(boat, store, thread, &due.ports, now, Some(SWEEP_PATIENCE));
        out.changed.extend(d.changed);
        out.failed |= d.failed;
        out.busy |= d.busy;
    }
    out
}

/// The worker's sweeps. Each look plans, hands each thread's ends to a worker of its own (at most
/// [`SWEEP_AT_ONCE`] under way) and returns at once: the clock is never held up by boat, so a
/// link that comes due while another's end waits on a slow VM still ends on time. A thread with
/// ends under way is passed by until they're done; `told` hears which threads' links changed.
pub struct Sweeper {
    boat: BoatClient,
    db: PathBuf,
    under_way: Arc<(Mutex<BTreeSet<String>>, Condvar)>,
    /// When each due thread was first seen due, kept across looks until its end finishes (then,
    /// still due, it joins at the back): first come, first served.
    queued: Arc<Mutex<BTreeMap<String, i64>>>,
    told: Arc<dyn Fn(BTreeSet<String>) + Send + Sync>,
}

impl Sweeper {
    pub fn new(
        boat: BoatClient,
        db: PathBuf,
        told: impl Fn(BTreeSet<String>) + Send + Sync + 'static,
    ) -> Sweeper {
        Sweeper {
            boat,
            db,
            under_way: Arc::default(),
            queued: Arc::default(),
            told: Arc::new(told),
        }
    }

    /// Start ending what's due at `now`. `busy` while ends are under way or waiting for room:
    /// look again soon.
    pub fn look(&self, now: i64) -> Swept {
        let Ok(store) = Store::open(&self.db) else {
            return Swept {
                failed: true,
                ..Default::default()
            };
        };
        let (work, next_ms) = plan(&store, now);
        let mut out = Swept {
            next_ms,
            ..Default::default()
        };
        // First come, first served across looks: neither a retry (back of the queue once its end
        // finishes) nor threads that came due since pass one that has waited longer, so ends
        // that keep failing, or new ones arriving, can't keep another link public.
        let mut work: Vec<(String, Due)> = work.into_iter().collect();
        {
            let mut queued = self.queued.lock().unwrap_or_else(|e| e.into_inner());
            queued.retain(|t, _| work.iter().any(|(w, _)| w == t));
            for (t, _) in &work {
                queued.entry(t.clone()).or_insert(now);
            }
            work.sort_by(|(a, _), (b, _)| queued.get(a).cmp(&queued.get(b)).then_with(|| a.cmp(b)));
        }
        let (set, freed) = &*self.under_way;
        let mut under_way = set.lock().unwrap_or_else(|e| e.into_inner());
        for (thread, Due { ports, .. }) in work {
            if under_way.contains(&thread) || under_way.len() >= SWEEP_AT_ONCE {
                out.busy = true;
                continue;
            }
            under_way.insert(thread.clone());
            let (boat, db, all, told, queued) = (
                self.boat.clone(),
                self.db.clone(),
                self.under_way.clone(),
                self.told.clone(),
                self.queued.clone(),
            );
            let name = thread.clone();
            let spawned = std::thread::Builder::new()
                .name("flotilla-public-end".into())
                .spawn(move || {
                    let d = match Store::open(&db) {
                        Ok(own) => sweep_thread(&boat, &own, &name, &ports, now, None),
                        Err(_) => Swept {
                            failed: true,
                            ..Default::default()
                        },
                    };
                    if !d.changed.is_empty() {
                        told(d.changed);
                    }
                    // Still due (it failed): back of the queue at the next look.
                    queued
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&name);
                    let (set, freed) = &*all;
                    set.lock().unwrap_or_else(|e| e.into_inner()).remove(&name);
                    freed.notify_all();
                });
            if spawned.is_err() {
                under_way.remove(&thread);
                freed.notify_all();
                out.failed = true;
            }
        }
        out.busy |= !under_way.is_empty();
        out
    }

    /// Wait (at most `within`) until no end is under way: whether none is.
    pub fn settle(&self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        let (set, freed) = &*self.under_way;
        let mut under_way = set.lock().unwrap_or_else(|e| e.into_inner());
        while !under_way.is_empty() {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            under_way = freed
                .wait_timeout(under_way, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }
}

/// How long `sweep` waits for a thread held by something else (a slow preview) before leaving
/// it for the next look. A [`Sweeper`]'s workers wait as long as it takes.
const SWEEP_PATIENCE: Duration = Duration::from_millis(250);

/// One thread's links, with the thread held (waiting at most `patience`, if given: left for the
/// next look if it's held longer).
fn sweep_thread(
    boat: &BoatClient,
    store: &Store,
    thread: &str,
    ports: &[u16],
    now: i64,
    patience: Option<Duration>,
) -> Swept {
    let mut out = Swept::default();
    // A rebuild since the connection opened: the next look, on the new database.
    let Ok(_gate) = changing(store) else {
        out.busy = true;
        return out;
    };
    let _held = match patience {
        None => hold(thread),
        Some(p) => match try_hold(thread, p) {
            Some(h) => h,
            None => {
                out.busy = true;
                return out;
            }
        },
    };
    for &port in ports {
        // As it is now, with the thread held: made public again, or private, since it was listed.
        let Some(r) = get(store, thread, port) else {
            continue;
        };
        let t = store.thread(thread).ok().flatten();
        let archived = store.is_archived(thread).unwrap_or(false);
        if !worth(&r, t.as_ref(), archived, now) {
            continue;
        }
        let was_shown = !r.ended;
        match end(boat, store, t.as_ref(), r.clone(), now) {
            Ok(_) => {
                if was_shown {
                    out.changed.insert(r.thread.clone());
                }
            }
            Err(e) => {
                out.failed = true;
                out.changed.insert(r.thread.clone());
                eprintln!(
                    "[public] {} port {}: couldn't make it private: {}",
                    r.thread,
                    r.port,
                    e.plain()
                );
            }
        }
    }
    out
}

#[cfg(any(test, feature = "testing"))]
pub mod testing {
    //! A stand-in for boat's host API: each port's access and token, the VMs stopped, and the
    //! calls made, as boat answers them (`design/public-links.md`).

    use std::collections::{BTreeMap, BTreeSet};
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    pub struct Boat {
        /// (sandbox, port) → its token while private; None while public.
        pub ports: BTreeMap<(String, u16), Option<u32>>,
        pub stopped: BTreeSet<String>,
        /// Each host call: (sandbox, port, public).
        pub calls: Vec<(String, u16, bool)>,
        /// Every call fails with a 500 while set.
        pub failing: bool,
        /// Calls for these VMs fail with a 500.
        pub failing_for: BTreeSet<String>,
        /// Making public is done, but its answer is lost (a 500) while set.
        pub lose_public_answers: bool,
        /// VMs deleted: a 404.
        pub gone: BTreeSet<String>,
        /// VMs whose host calls take this long to answer.
        pub slow: BTreeMap<String, std::time::Duration>,
        /// When each host call was answered: (sandbox, port, public, at).
        pub answered: Vec<(String, u16, bool, std::time::Instant)>,
        tokens: u32,
    }

    impl Boat {
        /// Whether anyone can open the port now.
        pub fn public(&self, sandbox: &str, port: u16) -> bool {
            matches!(self.ports.get(&(sandbox.to_string(), port)), Some(None))
        }
    }

    pub fn url(sandbox: &str, port: u16) -> String {
        format!("https://{sandbox}-{port}.on.boat.example")
    }

    /// A boat answering host calls on a local port, and what it holds.
    pub fn serve() -> (crate::boat::BoatClient, Arc<Mutex<Boat>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/api/v1", listener.local_addr().unwrap());
        let boat: Arc<Mutex<Boat>> = Arc::default();
        let held = boat.clone();
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                let held = held.clone();
                // Each on its own thread, as boat answers calls side by side.
                std::thread::spawn(move || {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    // Headers, then the body by its length.
                    loop {
                        let n = conn.read(&mut chunk).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf);
                        if let Some(h) = text.find("\r\n\r\n") {
                            let len = text[..h]
                                .lines()
                                .find_map(|l| {
                                    l.to_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                                })
                                .unwrap_or(0);
                            if buf.len() >= h + 4 + len {
                                break;
                            }
                        }
                    }
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                    let body: serde_json::Value = text
                        .split_once("\r\n\r\n")
                        .and_then(|(_, b)| serde_json::from_str(b).ok())
                        .unwrap_or_default();
                    let sandbox = path
                        .strip_prefix("/api/v1/sandboxes/")
                        .and_then(|p| p.strip_suffix("/host"))
                        .unwrap_or("")
                        .to_string();
                    let (port, public) = (
                        body["port"].as_u64().unwrap_or(0) as u16,
                        body["public"].as_bool().unwrap_or(false),
                    );
                    let (status, answer, slow) = {
                        let mut b = held.lock().unwrap();
                        let (status, answer) = answer(&mut b, &path, &body);
                        (status, answer, b.slow.get(&sandbox).copied())
                    };
                    if let Some(d) = slow {
                        std::thread::sleep(d);
                    }
                    let answer = answer.to_string();
                    let _ = write!(
                        conn,
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    );
                    held.lock().unwrap().answered.push((
                        sandbox,
                        port,
                        public,
                        std::time::Instant::now(),
                    ));
                });
            }
        });
        (
            crate::boat::BoatClient::with_base(
                "boat_test".into(),
                &base,
                &format!("{base}/provider-models"),
            )
            .unwrap(),
            boat,
        )
    }

    fn answer(b: &mut Boat, path: &str, body: &serde_json::Value) -> (u16, serde_json::Value) {
        let Some(sandbox) = path
            .strip_prefix("/api/v1/sandboxes/")
            .and_then(|p| p.strip_suffix("/host"))
        else {
            return (
                404,
                serde_json::json!({"ok": false, "error": {"code": "not_found", "message": "no such path", "status": 404}}),
            );
        };
        let port = body["port"].as_u64().unwrap_or(0) as u16;
        let public = body["public"].as_bool().unwrap_or(false);
        b.calls.push((sandbox.to_string(), port, public));
        let down = || {
            (
                500,
                serde_json::json!({"ok": false, "error": {"code": "internal", "message": "boat is down", "status": 500}}),
            )
        };
        if b.failing || b.failing_for.contains(sandbox) {
            return down();
        }
        if b.gone.contains(sandbox) {
            return (
                404,
                serde_json::json!({"ok": false, "error": {"code": "not_found", "message": "no such sandbox", "status": 404}}),
            );
        }
        if b.stopped.contains(sandbox) {
            let msg = format!("Sandbox {sandbox} is stopped.");
            return (
                400,
                serde_json::json!({"ok": false, "code": "machine_not_running", "error": {"code": "machine_not_running", "message": msg, "status": 400}}),
            );
        }
        let key = (sandbox.to_string(), port);
        let token = if public {
            None
        } else {
            match b.ports.get(&key) {
                // Hosting a private port again keeps its token; a public one gets a new one.
                Some(Some(t)) => Some(*t),
                _ => {
                    b.tokens += 1;
                    Some(b.tokens)
                }
            }
        };
        b.ports.insert(key, token);
        if public && b.lose_public_answers {
            return down();
        }
        let url = match token {
            Some(t) => format!("{}?_token=t{t}", url(sandbox, port)),
            None => url(sandbox, port),
        };
        let access = if public { "public" } else { "private" };
        (
            200,
            serde_json::json!({"ok": true, "type": "port.hosted", "success": true, "port": port, "url": url, "isProtected": !public, "access": access}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{self, Boat};
    use super::*;
    use std::sync::{Arc, Mutex};

    const HOUR: i64 = 3_600_000;

    fn thread(id: &str, sandbox: &str, state: ThreadState) -> Thread {
        Thread {
            id: id.into(),
            environment_id: Some("e1".into()),
            environment_name: Some("base".into()),
            title: "Web".into(),
            sandbox_id: Some(sandbox.into()),
            conversation_id: Some(format!("conv_{id}")),
            provider: "claude-code".into(),
            model: "claude-haiku-5-5".into(),
            effort: None,
            fast: false,
            state,
            keep_running: false,
            archive_after: None,
            last_stopped_at: None,
            data_expired_at: None,
            created_at: "2026-11-02T00:00:00Z".into(),
            updated_at: "2026-11-02T00:00:00Z".into(),
            repo_project_id: None,
            branch: None,
            base_sha: None,
        }
    }

    fn setup(
        name: &str,
    ) -> (
        std::path::PathBuf,
        Store,
        crate::boat::BoatClient,
        Arc<Mutex<Boat>>,
    ) {
        let dir =
            std::env::temp_dir().join(format!("flotilla-public-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("flotilla.db");
        let store = Store::open(&db).unwrap();
        let (boat, held) = testing::serve();
        (db, store, boat, held)
    }

    fn set_state(store: &Store, id: &str, state: ThreadState) -> Thread {
        store.set_thread_state(id, state).unwrap();
        store.thread(id).unwrap().unwrap()
    }

    #[test]
    fn a_link_made_public_opens_without_a_token_and_private_again_gets_a_new_one() {
        let (_db, store, boat, held) = setup("private");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        let first = host(&boat, &store, "t1", "bx_1", 3000).unwrap();
        assert!(first.ends_with("?_token=t1"), "{first}");
        let now = crate::util::now_ms();
        let url = make_public(&boat, &store, &t, 3000, PublicFor::Hour, now).unwrap();
        assert_eq!(url, testing::url("bx_1", 3000));
        assert!(held.lock().unwrap().public("bx_1", 3000));
        assert_eq!(
            shown(&store, &t),
            [PublicLink {
                port: 3000,
                url: url.clone(),
                since_ms: now,
                until_ms: Some(now + HOUR)
            }]
        );
        // Preview… on it now opens the public link, and leaves it public.
        assert_eq!(host(&boat, &store, "t1", "bx_1", 3000).unwrap(), url);
        assert!(held.lock().unwrap().public("bx_1", 3000));
        // Again, for longer: its time starts over, from when it first went public.
        make_public(&boat, &store, &t, 3000, PublicFor::Week, now + 5_000).unwrap();
        assert_eq!(
            shown(&store, &t)[0].until_ms,
            Some(now + 5_000 + 7 * 24 * HOUR)
        );
        assert_eq!(shown(&store, &t)[0].since_ms, now);
        make_private(&boat, &store, &t, 3000).unwrap();
        assert!(!held.lock().unwrap().public("bx_1", 3000));
        assert!(shown(&store, &t).is_empty());
        // A new token, kept for the services' links; Preview… opens with it.
        let newer = private_url(&store, "t1", "bx_1", 3000).unwrap();
        assert!(newer.ends_with("?_token=t2"), "{newer}");
        assert_eq!(host(&boat, &store, "t1", "bx_1", 3000).unwrap(), newer);
        // Not public: nothing to do, and boat isn't asked.
        let calls = held.lock().unwrap().calls.len();
        make_private(&boat, &store, &t, 4100).unwrap();
        assert_eq!(held.lock().unwrap().calls.len(), calls);
    }

    #[test]
    fn a_link_ends_at_its_time_even_after_a_restart_and_until_stopped_never_does() {
        let (db, store, boat, held) = setup("expiry");
        let t = thread("t1", "bx_1", ThreadState::Running);
        store.insert_thread(&t).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::Hour, 0).unwrap();
        make_public(&boat, &store, &t, 4100, PublicFor::UntilStopped, 0).unwrap();
        // Not yet: the next end is the hour's.
        let s = sweep(&boat, &store, HOUR - 1);
        assert_eq!(
            s,
            Swept {
                next_ms: Some(HOUR),
                ..Default::default()
            }
        );
        assert!(held.lock().unwrap().public("bx_1", 3000));
        // The server restarts: what's kept is all it needs.
        drop(store);
        let store = Store::open(&db).unwrap();
        let s = sweep(&boat, &store, HOUR);
        assert_eq!(s.changed, BTreeSet::from(["t1".to_string()]));
        assert_eq!(s.next_ms, None);
        assert!(!held.lock().unwrap().public("bx_1", 3000));
        // Until stopped: a year on, still public.
        let year = 365 * 24 * HOUR;
        assert_eq!(sweep(&boat, &store, year), Swept::default());
        assert!(held.lock().unwrap().public("bx_1", 4100));
        assert_eq!(
            shown(&store, &t)
                .iter()
                .map(|l| (l.port, l.until_ms))
                .collect::<Vec<_>>(),
            [(4100, None)]
        );
    }

    #[test]
    fn a_stopped_vm_s_link_ends_as_it_wakes_and_one_with_time_left_is_public_again() {
        let (_db, store, boat, held) = setup("stopped");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::Hour, 0).unwrap();
        make_public(&boat, &store, &t, 4100, PublicFor::Day, 0).unwrap();
        held.lock().unwrap().stopped.insert("bx_1".into());
        let t = set_state(&store, "t1", ThreadState::Stopped);
        // Its hour runs out while it's stopped: no longer shown, and boat (which can't be told)
        // isn't asked.
        let calls = held.lock().unwrap().calls.len();
        let s = sweep(&boat, &store, HOUR);
        assert_eq!(s.changed, BTreeSet::from(["t1".to_string()]));
        assert_eq!(held.lock().unwrap().calls.len(), calls);
        assert_eq!(
            shown(&store, &t).iter().map(|l| l.port).collect::<Vec<_>>(),
            [4100]
        );
        // Looked at again while stopped: nothing more.
        assert_eq!(sweep(&boat, &store, HOUR + 1).changed, BTreeSet::new());
        // It wakes: 3000 private, 4100 public again.
        held.lock().unwrap().stopped.clear();
        held.lock().unwrap().ports.clear();
        let t = set_state(&store, "t1", ThreadState::Idle);
        on_wake(&boat, &store, &t, HOUR + 2);
        assert!(!held.lock().unwrap().public("bx_1", 3000));
        assert!(
            held.lock()
                .unwrap()
                .ports
                .contains_key(&("bx_1".to_string(), 3000))
        );
        assert!(held.lock().unwrap().public("bx_1", 4100));
        assert_eq!(
            records(&store, Some("t1"))
                .iter()
                .map(|r| r.port)
                .collect::<Vec<_>>(),
            [4100]
        );
    }

    #[test]
    fn made_private_while_stopped_and_woken_outside_flotilla_the_sweep_ends_it() {
        let (_db, store, boat, held) = setup("outside");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::UntilStopped, 0).unwrap();
        held.lock().unwrap().stopped.insert("bx_1".into());
        // The VM stopped, but Flotilla hasn't seen it yet: boat says so, and it ends at the wake.
        make_private(&boat, &store, &t, 3000).unwrap();
        assert!(shown(&store, &t).is_empty());
        assert!(records(&store, Some("t1"))[0].ended);
        // Woken from boat's CLI: the next look ends it.
        held.lock().unwrap().stopped.clear();
        let s = sweep(&boat, &store, 1);
        assert!(s.changed.is_empty(), "it wasn't shown");
        assert!(!held.lock().unwrap().public("bx_1", 3000));
        assert!(records(&store, Some("t1")).is_empty());
    }

    #[test]
    fn a_failure_leaves_the_link_public_and_shown_until_boat_takes_it() {
        let (_db, store, boat, held) = setup("failing");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::Hour, 0).unwrap();
        held.lock().unwrap().failing = true;
        assert!(make_private(&boat, &store, &t, 3000).is_err());
        assert_eq!(shown(&store, &t).len(), 1, "still public: shown as public");
        let s = sweep(&boat, &store, HOUR);
        assert!(s.failed);
        assert_eq!(shown(&store, &t).len(), 1);
        held.lock().unwrap().failing = false;
        assert_eq!(
            sweep(&boat, &store, HOUR + 1).changed,
            BTreeSet::from(["t1".to_string()])
        );
        assert!(!held.lock().unwrap().public("bx_1", 3000));
        // Making it private failed: it shows as ending (due now), and the next look ends it.
        held.lock().unwrap().failing = true;
        make_public(&boat, &store, &t, 3000, PublicFor::UntilStopped, 0).ok();
        held.lock().unwrap().failing = false;
        make_public(&boat, &store, &t, 3000, PublicFor::UntilStopped, 0).unwrap();
        held.lock().unwrap().failing = true;
        assert!(make_private(&boat, &store, &t, 3000).is_err());
        let left = shown(&store, &t);
        assert!(left.len() == 1 && left[0].until_ms.is_some(), "{left:?}");
        held.lock().unwrap().failing = false;
        assert!(
            sweep(&boat, &store, crate::util::now_ms())
                .changed
                .contains("t1")
        );
        assert!(
            !held.lock().unwrap().public("bx_1", 3000) && records(&store, Some("t1")).is_empty()
        );
    }

    /// Kept before boat is asked: an answer lost on the way is put back to private; with boat
    /// down it shows as ending until boat confirms; with nothing kept, boat isn't asked.
    #[test]
    fn a_link_boat_may_have_made_public_is_never_left_untracked() {
        let (db, store, boat, held) = setup("untracked");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        host(&boat, &store, "t1", "bx_1", 4100).unwrap();
        // boat made it public, but the answer was lost: put back.
        held.lock().unwrap().lose_public_answers = true;
        assert!(make_public(&boat, &store, &t, 4100, PublicFor::Hour, 0).is_err());
        assert!(!held.lock().unwrap().public("bx_1", 4100));
        assert!(records(&store, Some("t1")).is_empty());
        assert!(
            private_url(&store, "t1", "bx_1", 4100)
                .unwrap()
                .contains("_token=")
        );
        // boat down: maybe public, so shown, ending, under the URL known for the port.
        held.lock().unwrap().lose_public_answers = false;
        held.lock().unwrap().failing = true;
        assert!(make_public(&boat, &store, &t, 4100, PublicFor::Week, 0).is_err());
        let left = shown(&store, &t);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].url, testing::url("bx_1", 4100));
        assert!(
            left[0].until_ms.unwrap() <= crate::util::now_ms(),
            "due now, not in a week"
        );
        held.lock().unwrap().failing = false;
        sweep(&boat, &store, crate::util::now_ms());
        assert!(
            records(&store, Some("t1")).is_empty() && !held.lock().unwrap().public("bx_1", 4100)
        );
        // Public already, and boat fails: it stays as it was.
        make_public(&boat, &store, &t, 4100, PublicFor::Hour, 0).unwrap();
        held.lock().unwrap().failing = true;
        assert!(make_public(&boat, &store, &t, 4100, PublicFor::Week, 0).is_err());
        assert_eq!(shown(&store, &t)[0].until_ms, Some(HOUR));
        held.lock().unwrap().failing = false;
        // Nothing can be kept: boat isn't asked.
        let calls = held.lock().unwrap().calls.len();
        let read_only = Store::open_read_only(&db).unwrap();
        assert!(make_public(&boat, &read_only, &t, 3000, PublicFor::Hour, 0).is_err());
        assert_eq!(held.lock().unwrap().calls.len(), calls);
        assert!(!held.lock().unwrap().public("bx_1", 3000));
    }

    /// Preview (anyone's, an agent's too) of a link past its time hosts it private, before the
    /// worker has looked.
    #[test]
    fn a_preview_of_a_link_past_its_time_makes_it_private() {
        let (_db, store, boat, held) = setup("preview-late");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::Hour, 0).unwrap();
        let url = host(&boat, &store, "t1", "bx_1", 3000).unwrap();
        assert!(url.contains("_token="), "{url}");
        assert!(!held.lock().unwrap().public("bx_1", 3000));
        assert!(records(&store, Some("t1")).is_empty());
    }

    /// Archiving with boat failing on a VM that's up: still shown, ending, and tried again;
    /// once the VM stops it's hidden, and private as it wakes.
    #[test]
    fn archiving_hides_a_link_only_once_boat_or_a_stop_has_closed_it() {
        let (_db, store, boat, held) = setup("archive-fail");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::UntilStopped, 0).unwrap();
        held.lock().unwrap().failing = true;
        end_all(&boat, &store, &t);
        assert_eq!(
            shown(&store, &t).len(),
            1,
            "still public as far as anyone knows"
        );
        let s = sweep(&boat, &store, crate::util::now_ms());
        assert!(s.failed, "tried again at every look");
        // The archive stops the VM: hidden, private as it wakes.
        held.lock().unwrap().failing = false;
        held.lock().unwrap().stopped.insert("bx_1".into());
        let t = set_state(&store, "t1", ThreadState::Stopped);
        sweep(&boat, &store, crate::util::now_ms());
        assert!(shown(&store, &t).is_empty());
        held.lock().unwrap().stopped.clear();
        let t = set_state(&store, "t1", ThreadState::Idle);
        on_wake(&boat, &store, &t, crate::util::now_ms());
        assert!(
            !held.lock().unwrap().public("bx_1", 3000) && records(&store, Some("t1")).is_empty()
        );
    }

    /// A rebuilt database carries the links over; one whose thread isn't back yet is kept until
    /// due, then ended through boat alone (a VM that's gone is forgotten).
    #[test]
    fn a_rebuild_keeps_the_links_and_ends_them_on_time() {
        let (db, store, boat, held) = setup("rebuild");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        let t2 = thread("t2", "bx_2", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        store.insert_thread(&t2).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::Hour, 0).unwrap();
        make_public(&boat, &store, &t2, 3000, PublicFor::Hour, 0).unwrap();
        drop(store);
        Store::backup(&db).unwrap();
        let store = Store::open(&db).unwrap();
        assert!(
            store.thread("t1").unwrap().is_none(),
            "threads come back from boat later"
        );
        assert_eq!(records(&store, None).len(), 2);
        // Not due: kept, with the next end known.
        assert_eq!(
            sweep(&boat, &store, HOUR - 1),
            Swept {
                next_ms: Some(HOUR),
                ..Default::default()
            }
        );
        assert_eq!(records(&store, None).len(), 2);
        // Due: boat makes t1's private; t2's VM is gone.
        held.lock().unwrap().gone.insert("bx_2".into());
        sweep(&boat, &store, HOUR);
        assert!(!held.lock().unwrap().public("bx_1", 3000));
        assert!(records(&store, None).is_empty());
    }

    /// A connection opened before a rebuild has the database moved aside: a link made public
    /// through it is kept in the database in use, so it still ends on time. Until the rebuild
    /// has the thread back, none is made.
    #[test]
    fn a_link_made_public_through_an_old_connection_is_kept_where_it_counts() {
        let (db, store, boat, held) = setup("moved-aside");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        Store::backup(&db).unwrap();
        assert!(store.stale());
        assert!(make_public(&boat, &store, &t, 3000, PublicFor::Hour, 0).is_err());
        assert!(held.lock().unwrap().calls.is_empty());
        let fresh = Store::open(&db).unwrap();
        fresh.insert_thread(&t).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::Hour, 0).unwrap();
        assert!(!fresh.stale());
        assert_eq!(
            records(&fresh, None).len(),
            1,
            "kept in the database in use"
        );
        assert!(records(&store, None).is_empty(), "not the one moved aside");
        // It ends on time from there.
        sweep(&boat, &fresh, HOUR);
        assert!(records(&fresh, None).is_empty());
    }

    /// A thread whose lock is held long (a slow preview) is left for the next look; another
    /// thread's link still ends on time.
    #[test]
    fn a_slow_thread_holds_up_no_other_threads_end() {
        let (_db, store, boat, held) = setup("slow");
        let (a, b) = ("slow-a".to_string(), "slow-b".to_string());
        let ta = thread(&a, "bx_a", ThreadState::Idle);
        let tb = thread(&b, "bx_b", ThreadState::Idle);
        store.insert_thread(&ta).unwrap();
        store.insert_thread(&tb).unwrap();
        make_public(&boat, &store, &ta, 3000, PublicFor::Hour, 0).unwrap();
        make_public(&boat, &store, &tb, 3000, PublicFor::Hour, 0).unwrap();
        let busy = hold(&a);
        let s = sweep(&boat, &store, HOUR);
        drop(busy);
        assert!(s.busy, "a's link is left for the next look");
        assert!(held.lock().unwrap().public("bx_a", 3000));
        assert!(
            !held.lock().unwrap().public("bx_b", 3000),
            "b's ends on time"
        );
        assert_eq!(s.changed, BTreeSet::from([b]));
        let s = sweep(&boat, &store, HOUR);
        assert!(!s.busy && !held.lock().unwrap().public("bx_a", 3000));
    }

    /// Every path that hosts a port or ends a link reads the database in use, not one moved
    /// aside (Preview, a wake, archiving): a link made private there isn't reopened by what an old
    /// connection still says.
    #[test]
    fn an_old_connection_never_reopens_a_link() {
        let (db, store, boat, held) = setup("stale-host");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        make_public(
            &boat,
            &store,
            &t,
            3000,
            PublicFor::Hour,
            crate::util::now_ms(),
        )
        .unwrap();
        Store::backup(&db).unwrap();
        let fresh = Store::open(&db).unwrap();
        fresh.insert_thread(&t).unwrap();
        make_private(&boat, &fresh, &t, 3000).unwrap();
        // The old connection still says public: nothing reopens it.
        assert!(
            host(&boat, &store, "t1", "bx_1", 3000)
                .unwrap()
                .contains("_token=")
        );
        on_wake(&boat, &store, &t, crate::util::now_ms());
        end_all(&boat, &store, &t);
        assert!(
            !held
                .lock()
                .unwrap()
                .calls
                .iter()
                .skip(2)
                .any(|(_, _, public)| *public),
            "{:?}",
            held.lock().unwrap().calls
        );
        assert!(!held.lock().unwrap().public("bx_1", 3000));
    }

    /// An archived thread keeps nothing public, however archiving was cut off (a crash, a rebuild
    /// between marking it archived and ending its links): the worker ends them, and a wake (for
    /// `.agents/archive`) doesn't host them public again.
    #[test]
    fn an_archived_thread_keeps_nothing_public_however_archiving_was_cut_off() {
        let (_db, store, boat, held) = setup("archived");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::UntilStopped, 0).unwrap();
        make_public(&boat, &store, &t, 4100, PublicFor::UntilStopped, 0).unwrap();
        store.set_archived("t1", true).unwrap();
        // Woken for `.agents/archive` before the links were ended: 4100 isn't hosted public again.
        held.lock().unwrap().calls.clear();
        on_wake(&boat, &store, &t, 0);
        assert!(
            held.lock()
                .unwrap()
                .calls
                .iter()
                .all(|(_, _, public)| !public)
        );
        assert!(
            !held.lock().unwrap().public("bx_1", 4100)
                && !held.lock().unwrap().public("bx_1", 3000)
        );
        // And with no wake, the worker ends them.
        make_public(&boat, &store, &t, 5000, PublicFor::UntilStopped, 0).unwrap();
        assert!(sweep(&boat, &store, 1).changed.contains("t1"));
        assert!(!held.lock().unwrap().public("bx_1", 5000));
        assert!(records(&store, Some("t1")).is_empty());
    }

    /// Archiving cut off after its links were made due (before they were ended), then a rebuild,
    /// which carries the links but not the archive: they still end, unarchived or not.
    #[test]
    fn archiving_cut_off_then_a_rebuild_still_ends_its_links() {
        let (db, store, boat, held) = setup("archive-rebuild");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::UntilStopped, 0).unwrap();
        let now = crate::util::now_ms();
        closing(&store, "t1", now).unwrap();
        store.set_archived("t1", true).unwrap();
        // Cut off here: no end_all. Then a rebuild.
        Store::backup(&db).unwrap();
        let fresh = Store::open(&db).unwrap();
        assert!(
            !fresh.is_archived("t1").unwrap(),
            "a rebuild doesn't carry the archive"
        );
        assert_eq!(
            records(&fresh, None)[0].until_ms,
            Some(now),
            "but the link is due"
        );
        sweep(&boat, &fresh, now);
        assert!(!held.lock().unwrap().public("bx_1", 3000));
        assert!(records(&fresh, None).is_empty());
        // Unarchiving doesn't take it back; making it public again does.
        let t = thread("t2", "bx_2", ThreadState::Idle);
        fresh.insert_thread(&t).unwrap();
        make_public(&boat, &fresh, &t, 3000, PublicFor::UntilStopped, 0).unwrap();
        closing(&fresh, "t2", now).unwrap();
        fresh.set_archived("t2", false).unwrap();
        assert!(records(&fresh, Some("t2"))[0].due(now));
        make_public(&boat, &fresh, &t, 3000, PublicFor::UntilStopped, now).unwrap();
        assert_eq!(records(&fresh, Some("t2"))[0].until_ms, None);
        // Preview of an archived thread's link hosts it private.
        fresh.set_archived("t2", true).unwrap();
        assert!(
            host(&boat, &fresh, "t2", "bx_2", 3000)
                .unwrap()
                .contains("_token=")
        );
        assert!(!held.lock().unwrap().public("bx_2", 3000));
    }

    /// Ending a link after a rebuild goes by the thread as the database in use has it, not the
    /// caller's copy from the one moved aside: that one says Stopped (or another VM), this one
    /// says up, so boat is asked and the link closed.
    #[test]
    fn ending_after_a_rebuild_goes_by_the_thread_in_use() {
        let (db, store, boat, held) = setup("thread-now");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::UntilStopped, 0).unwrap();
        make_public(&boat, &store, &t, 4100, PublicFor::UntilStopped, 0).unwrap();
        let old_stopped = set_state(&store, "t1", ThreadState::Stopped);
        Store::backup(&db).unwrap();
        let fresh = Store::open(&db).unwrap();
        fresh.insert_thread(&t).unwrap();
        make_private(&boat, &store, &old_stopped, 3000).unwrap();
        assert!(
            !held.lock().unwrap().public("bx_1", 3000),
            "boat was asked, not skipped"
        );
        assert!(records(&fresh, None).iter().all(|r| r.port != 3000));
        // A copy on another VM doesn't make the link in use an orphan.
        let mut elsewhere = t.clone();
        elsewhere.sandbox_id = Some("bx_9".into());
        end_all(&boat, &store, &elsewhere);
        assert!(!held.lock().unwrap().public("bx_1", 4100));
        assert!(records(&fresh, None).is_empty());
    }

    /// Several opens at once of a database whose swap was cut off: one finishes it, alone; the
    /// rest open the new database, and what each writes is there (no WAL of it moved aside).
    #[test]
    fn opens_at_once_finish_a_cut_off_swap_once() {
        let (db, store, boat, _held) = setup("swap-race");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::Week, 0).unwrap();
        drop(store);
        Store::prepare_rebuilt(&db).unwrap();
        let bak = db.with_extension("db.bak");
        std::fs::write(
            format!("{}.rebuilt-old", db.display()),
            bak.to_string_lossy().as_bytes(),
        )
        .unwrap();
        std::fs::rename(&db, &bak).unwrap();
        let openers: Vec<_> = (0..8)
            .map(|i| {
                let db = db.clone();
                std::thread::spawn(move || {
                    let s = Store::open(&db).unwrap();
                    s.kv_set(&format!("wrote:{i}"), "yes").unwrap();
                })
            })
            .collect();
        openers.into_iter().for_each(|h| h.join().unwrap());
        let s = Store::open(&db).unwrap();
        let ok: String = s
            .conn()
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ok, "ok");
        assert_eq!(records(&s, None).len(), 1);
        assert_eq!(s.kv_with_prefix("wrote:").unwrap().len(), 8);
        assert!(!std::path::PathBuf::from(format!("{}-wal.orphaned", db.display())).exists());
        assert!(
            !std::path::PathBuf::from(format!("{}-wal", bak.display())).exists(),
            "no WAL of the new database went with the old"
        );
    }

    /// A swap cut off with the old database's WAL left behind (a reader held its checkpoint off):
    /// the WAL goes with the old database, never into the new one, which opens whole.
    #[test]
    fn a_swap_cut_off_never_reads_the_old_wal_into_the_new_database() {
        let (db, store, boat, _held) = setup("swap-wal");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::Week, 0).unwrap();
        for i in 0..50 {
            store
                .kv_set(&format!("filler:{i}"), &"x".repeat(2000))
                .unwrap();
        }
        let wal = std::path::PathBuf::from(format!("{}-wal", db.display()));
        // Its WAL as a reader holding the checkpoint off would leave it: pages not in the file.
        let old_wal = std::fs::read(&wal).unwrap();
        assert!(
            old_wal.len() > 4096,
            "the old database has a WAL with pages in it"
        );
        let rebuilt = Store::prepare_rebuilt(&db).unwrap();
        assert!(
            !std::path::PathBuf::from(format!("{}-wal", rebuilt.display())).exists(),
            "the new one is one file"
        );
        // Cut off: the journal written and the old main file moved, its WAL not yet.
        let bak = db.with_extension("db.bak");
        std::fs::write(
            format!("{}.rebuilt-old", db.display()),
            bak.to_string_lossy().as_bytes(),
        )
        .unwrap();
        std::fs::copy(&db, &bak).unwrap();
        drop(store);
        std::fs::remove_file(&db).unwrap();
        std::fs::write(&wal, &old_wal).unwrap();
        assert!(!db.exists() && std::fs::metadata(&wal).unwrap().len() > 4096);
        let opened = Store::open(&db).unwrap();
        let ok: String = opened
            .conn()
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ok, "ok");
        assert_eq!(records(&opened, None).len(), 1);
        assert!(
            opened.kv_get("filler:0").unwrap().is_none(),
            "nothing of the old WAL came in"
        );
        assert!(
            std::path::PathBuf::from(format!("{}-wal", bak.display())).exists(),
            "it went with the old database"
        );
    }

    /// A rebuild cut off between moving the old database aside and putting the new one in its
    /// place: the next open puts it there, links and all. One cut off before the new one was
    /// whole leaves the old one as it was.
    #[test]
    fn a_rebuild_cut_off_midway_loses_no_link() {
        let (db, store, boat, _held) = setup("cut-off");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::Week, 0).unwrap();
        drop(store);
        // Whole beside it, then the old one moved aside, then the crash.
        let rebuilt = Store::prepare_rebuilt(&db).unwrap();
        std::fs::rename(&db, db.with_extension("db.bak")).unwrap();
        assert!(rebuilt.exists() && !db.exists());
        let store = Store::open(&db).unwrap();
        assert_eq!(records(&store, None).len(), 1, "carried over");
        assert!(!rebuilt.exists());
        drop(store);
        // Cut off as the new one was made: a half-made one is never taken.
        std::fs::write(format!("{}.rebuilding", db.display()), b"half").unwrap();
        let store = Store::open(&db).unwrap();
        assert_eq!(records(&store, None).len(), 1);
    }

    /// A connection can't open while a rebuild is under way: it waits, and opens the new one.
    #[test]
    fn opening_waits_for_a_rebuild() {
        let (db, store, _boat, _held) = setup("open-waits");
        drop(store);
        let gate = crate::store::gate_write(&db);
        let (tx, rx) = std::sync::mpsc::channel();
        let path = db.clone();
        let opener = std::thread::spawn(move || {
            let s = Store::open(&path).unwrap();
            tx.send(()).unwrap();
            s.stale()
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "it waits"
        );
        drop(gate);
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(!opener.join().unwrap());
    }

    /// The worker's look never waits for boat: with one thread's end waiting on a slow VM, a link
    /// that comes due a second later ends on time, before the slow call is answered. (Without
    /// the fix, the first look held the clock until the slow call came back.)
    #[test]
    fn a_link_due_while_another_waits_on_a_slow_vm_ends_on_time() {
        let (db, store, boat, held) = setup("due-while-slow");
        let ta = thread("due-a", "bx_due_a", ThreadState::Idle);
        let tb = thread("due-b", "bx_due_b", ThreadState::Idle);
        store.insert_thread(&ta).unwrap();
        store.insert_thread(&tb).unwrap();
        make_public(&boat, &store, &ta, 3000, PublicFor::Hour, 0).unwrap();
        make_public(&boat, &store, &tb, 3000, PublicFor::Hour, 1_000).unwrap();
        held.lock()
            .unwrap()
            .slow
            .insert("bx_due_a".into(), Duration::from_secs(2));
        let sweeper = Sweeper::new(boat.clone(), db.clone(), |_| {});
        let start = Instant::now();
        // a is due: its end starts, and the look comes straight back.
        let s = sweeper.look(HOUR);
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "the look waited for boat: {:?}",
            start.elapsed()
        );
        assert_eq!(s.next_ms, Some(HOUR + 1_000), "b's end is the next");
        assert!(s.busy, "an end under way: look again soon");
        // b's time comes while a's call is still out.
        let s = sweeper.look(HOUR + 1_000);
        assert!(s.busy && start.elapsed() < Duration::from_secs(1));
        assert!(sweeper.settle(Duration::from_secs(10)));
        let b = held.lock().unwrap();
        let answered = |sb: &str| {
            b.answered
                .iter()
                .find(|(s, _, public, at)| s == sb && !public && *at >= start)
                .map(|(.., at)| *at)
                .unwrap()
        };
        let (a_at, b_at) = (answered("bx_due_a"), answered("bx_due_b"));
        assert!(b_at < a_at, "b ended while a's call was out");
        assert!(
            b_at.duration_since(start) < Duration::from_secs(1),
            "b ended on time: {:?}",
            b_at.duration_since(start)
        );
        assert!(!b.public("bx_due_a", 3000) && !b.public("bx_due_b", 3000));
        drop(b);
        assert!(records(&store, None).is_empty());
    }

    /// Ends that keep failing never hold every place: with SWEEP_AT_ONCE threads failing at
    /// every try and one more due, healthy, and last in order, the healthy one goes at the next
    /// look, ahead of the retries. (Without the fix, the same failing ones took every place at
    /// every look, and it stayed public.)
    #[test]
    fn ends_that_keep_failing_never_starve_a_healthy_one() {
        let (db, store, boat, held) = setup("starve");
        let failing: Vec<String> = (0..SWEEP_AT_ONCE).map(|i| format!("starve-a{i}")).collect();
        for id in failing
            .iter()
            .chain(["starve-z-healthy".to_string()].iter())
        {
            let t = thread(id, &format!("bx_{id}"), ThreadState::Idle);
            store.insert_thread(&t).unwrap();
            make_public(&boat, &store, &t, 3000, PublicFor::Hour, 0).unwrap();
        }
        held.lock().unwrap().failing_for = failing.iter().map(|id| format!("bx_{id}")).collect();
        let sweeper = Sweeper::new(boat.clone(), db.clone(), |_| {});
        for look in 0..3 {
            sweeper.look(HOUR + look);
            assert!(sweeper.settle(Duration::from_secs(10)));
        }
        let b = held.lock().unwrap();
        assert!(
            !b.public("bx_starve-z-healthy", 3000),
            "the healthy one ended"
        );
        assert!(
            failing.iter().all(|id| b.public(&format!("bx_{id}"), 3000)),
            "the failing ones are still being tried"
        );
        drop(b);
        assert!(
            records(&store, None)
                .iter()
                .all(|r| r.thread != "starve-z-healthy")
        );
    }

    /// First come, first served: a link due first ends before threads that came due after it,
    /// however many keep arriving (each look a full batch that sorts ahead of it by id).
    #[test]
    fn a_link_due_first_is_not_passed_by_later_arrivals() {
        let (db, store, boat, held) = setup("fifo");
        let old = thread("fifo-z-old", "bx_fifo-z-old", ThreadState::Idle);
        store.insert_thread(&old).unwrap();
        make_public(&boat, &store, &old, 3000, PublicFor::Hour, 0).unwrap();
        let sweeper = Sweeper::new(boat.clone(), db.clone(), |_| {});
        for batch in 0..3_i64 {
            for i in 0..SWEEP_AT_ONCE {
                let id = format!("fifo-a{batch}-{i}");
                let t = thread(&id, &format!("bx_{id}"), ThreadState::Idle);
                store.insert_thread(&t).unwrap();
                make_public(&boat, &store, &t, 3000, PublicFor::Hour, batch).unwrap();
            }
            sweeper.look(HOUR + batch);
            assert!(sweeper.settle(Duration::from_secs(10)));
        }
        assert!(
            !held.lock().unwrap().public("bx_fifo-z-old", 3000),
            "the first one due ended"
        );
    }

    /// At most SWEEP_AT_ONCE threads' ends are under way; a thread under way is passed by, and
    /// the rest go at the next look, none waiting on the slow one.
    #[test]
    fn the_worker_ends_a_few_threads_at_once_and_the_rest_next() {
        let (db, store, boat, held) = setup("at-once");
        let ids: Vec<String> = (0..SWEEP_AT_ONCE + 2)
            .map(|i| format!("at-once-{i}"))
            .collect();
        for id in &ids {
            let t = thread(id, &format!("bx_{id}"), ThreadState::Idle);
            store.insert_thread(&t).unwrap();
            make_public(&boat, &store, &t, 3000, PublicFor::Hour, 0).unwrap();
        }
        let slow = format!("bx_{}", ids[0]);
        held.lock()
            .unwrap()
            .slow
            .insert(slow.clone(), Duration::from_secs(2));
        let told = Arc::new(Mutex::new(BTreeSet::new()));
        let heard = told.clone();
        let sweeper = Sweeper::new(boat.clone(), db.clone(), move |changed| {
            heard.lock().unwrap().extend(changed)
        });
        let start = Instant::now();
        sweeper.look(HOUR);
        assert!(sweeper.under_way.0.lock().unwrap().len() <= SWEEP_AT_ONCE);
        // The fast ones finish; the next look starts the rest, passing the slow one by.
        std::thread::sleep(Duration::from_millis(400));
        sweeper.look(HOUR);
        assert!(sweeper.settle(Duration::from_secs(10)));
        let b = held.lock().unwrap();
        for (sb, _, _, at) in b
            .answered
            .iter()
            .filter(|(_, _, public, at)| !public && *at >= start)
        {
            if *sb != slow {
                assert!(
                    at.duration_since(start) < Duration::from_secs(1),
                    "{sb} waited for the slow VM"
                );
            }
        }
        assert_eq!(
            b.calls
                .iter()
                .filter(|(sb, _, public)| *sb == slow && !public)
                .count(),
            1,
            "the slow one's end started once"
        );
        assert!(ids.iter().all(|id| !b.public(&format!("bx_{id}"), 3000)));
        assert_eq!(told.lock().unwrap().len(), ids.len());
    }

    /// Stopping (or starting, or in error) isn't stopped: boat is asked, and only boat saying
    /// so (or the VM stopped) hides a link.
    #[test]
    fn only_a_stopped_vm_is_taken_as_closed_without_asking_boat() {
        let (_db, store, boat, held) = setup("stopping");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::UntilStopped, 0).unwrap();
        make_public(&boat, &store, &t, 4100, PublicFor::UntilStopped, 0).unwrap();
        // Stopping, still serving: made private by boat.
        let t = set_state(&store, "t1", ThreadState::Stopping);
        make_private(&boat, &store, &t, 3000).unwrap();
        assert!(!held.lock().unwrap().public("bx_1", 3000));
        assert!(records(&store, Some("t1")).iter().all(|r| r.port != 3000));
        // Stopped: boat isn't asked; hidden, private as it wakes.
        held.lock().unwrap().stopped.insert("bx_1".into());
        let t = set_state(&store, "t1", ThreadState::Stopped);
        let calls = held.lock().unwrap().calls.len();
        make_private(&boat, &store, &t, 4100).unwrap();
        assert_eq!(held.lock().unwrap().calls.len(), calls);
        assert!(shown(&store, &t).is_empty());
        // Starting: boat is asked now and then, and says it's stopped (still hidden).
        let t = set_state(&store, "t1", ThreadState::Starting);
        sweep(&boat, &store, crate::util::now_ms() + RETRY_MS);
        assert_eq!(held.lock().unwrap().calls.len(), calls + 1);
        assert!(shown(&store, &t).is_empty());
    }

    #[test]
    fn archiving_ends_every_link_and_a_new_vm_or_a_deleted_thread_has_none() {
        let (_db, store, boat, held) = setup("archive");
        let t = thread("t1", "bx_1", ThreadState::Idle);
        store.insert_thread(&t).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::UntilStopped, 0).unwrap();
        make_public(&boat, &store, &t, 4100, PublicFor::Week, 0).unwrap();
        end_all(&boat, &store, &t);
        assert!(
            !held.lock().unwrap().public("bx_1", 3000)
                && !held.lock().unwrap().public("bx_1", 4100)
        );
        assert!(records(&store, Some("t1")).is_empty());
        // A thread now on another VM (a fork, a rebuild): the old VM's link is ended there,
        // then forgotten.
        make_public(&boat, &store, &t, 3000, PublicFor::Hour, 0).unwrap();
        let mut moved = t.clone();
        moved.sandbox_id = Some("bx_2".into());
        store.update_thread(&moved).unwrap();
        assert!(shown(&store, &moved).is_empty());
        assert_eq!(
            sweep(&boat, &store, 1).changed,
            BTreeSet::from(["t1".to_string()])
        );
        assert!(records(&store, Some("t1")).is_empty());
        assert!(
            !held.lock().unwrap().public("bx_1", 3000),
            "the old VM is told"
        );
        // The new VM gone says nothing of the old one's link.
        store.update_thread(&t).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::Hour, 0).unwrap();
        store
            .update_thread(&Thread {
                state: ThreadState::Expired,
                ..moved.clone()
            })
            .unwrap();
        sweep(&boat, &store, 1);
        assert!(
            !held.lock().unwrap().public("bx_1", 3000) && records(&store, Some("t1")).is_empty()
        );
        // Making the same port public on the new VM ends the old VM's link first, and is
        // refused while that can't be done (the record kept).
        store.update_thread(&t).unwrap();
        make_public(&boat, &store, &t, 3000, PublicFor::Hour, 0).unwrap();
        store.update_thread(&moved).unwrap();
        held.lock().unwrap().failing_for.insert("bx_1".into());
        assert!(make_public(&boat, &store, &moved, 3000, PublicFor::Hour, 2).is_err());
        assert!(
            held.lock().unwrap().public("bx_1", 3000) && !held.lock().unwrap().public("bx_2", 3000)
        );
        assert_eq!(
            records(&store, Some("t1"))
                .iter()
                .map(|r| r.sandbox.as_str())
                .collect::<Vec<_>>(),
            ["bx_1"]
        );
        held.lock().unwrap().failing_for.clear();
        make_public(&boat, &store, &moved, 3000, PublicFor::Hour, 3).unwrap();
        assert!(
            !held.lock().unwrap().public("bx_1", 3000) && held.lock().unwrap().public("bx_2", 3000)
        );
        assert_eq!(
            records(&store, Some("t1"))
                .iter()
                .map(|r| r.sandbox.as_str())
                .collect::<Vec<_>>(),
            ["bx_2"]
        );
        end_all(&boat, &store, &moved);
        // Deleting or forgetting a thread forgets its private URLs; its links stay, due, till
        // boat has them private (forgetting leaves the VM up), and none is made public since.
        let t2 = thread("t2", "bx_3", ThreadState::Idle);
        store.insert_thread(&t2).unwrap();
        make_public(&boat, &store, &t2, 3000, PublicFor::Hour, 0).unwrap();
        make_private(&boat, &store, &t2, 3000).unwrap();
        make_public(&boat, &store, &t2, 3000, PublicFor::UntilStopped, 0).unwrap();
        {
            let _busy = hold("t2");
            assert!(
                forgetting(&store, "t2", 5, Some(Duration::ZERO), |s| s
                    .delete_thread("t2"))
                .is_err(),
                "a link being changed: not now"
            );
            assert!(store.thread("t2").unwrap().is_some());
        }
        forgetting(&store, "t2", 5, Some(Duration::ZERO), |s| {
            s.delete_thread("t2")
        })
        .unwrap();
        assert_eq!(private_url(&store, "t2", "bx_3", 3000), None);
        assert_eq!(
            records(&store, Some("t2"))
                .iter()
                .map(|r| r.until_ms)
                .collect::<Vec<_>>(),
            [Some(5)]
        );
        assert!(make_public(&boat, &store, &t2, 4100, PublicFor::Hour, 6).is_err());
        assert!(
            held.lock().unwrap().public("bx_3", 3000) && !held.lock().unwrap().public("bx_3", 4100)
        );
        assert_eq!(
            sweep(&boat, &store, 6).changed,
            BTreeSet::from(["t2".to_string()])
        );
        assert!(!held.lock().unwrap().public("bx_3", 3000));
        assert!(records(&store, Some("t2")).is_empty());
    }

    #[test]
    fn the_test_knob_only_ever_shortens_a_timed_link() {
        assert_eq!(until_with(PublicFor::Hour, 0, Some("60")), Some(60_000));
        assert_eq!(until_with(PublicFor::Week, 7, Some(" 60 ")), Some(60_007));
        assert_eq!(until_with(PublicFor::UntilStopped, 0, Some("60")), None);
        assert_eq!(
            until_with(PublicFor::Hour, 0, Some("5")),
            Some(HOUR),
            "under 10 s is ignored"
        );
        assert_eq!(
            until_with(PublicFor::Hour, 0, Some("999999")),
            Some(HOUR),
            "never longer"
        );
        assert_eq!(until_with(PublicFor::Day, 0, Some("soon")), Some(24 * HOUR));
        assert_eq!(until_with(PublicFor::Day, 0, None), Some(24 * HOUR));
    }
}
