//! Agents handing work to other agents, or asking you. The `flotilla` command in a thread's VM
//! (`flotilla.sh`) prints a request line in its tool output; Flotilla, following the thread,
//! carries it out: `delegate` starts a child thread (a VM of its own, like New thread), `message`
//! sends a child a prompt, `cancel` stops its turn, `ask` puts a question for you in the thread
//! (`crate::asks`). When a child's turn that its parent asked for ends, Flotilla sends the parent
//! the child's answer, waking it.
//!
//! **Only the thread's own VM can ask.** A request line is plain text in a tool's output, and
//! so is anything else a tool prints: a PR comment `gh` read, a file, an install script's
//! output, a web page. So every thread has a key of its own (`kv`, never shown or sent to a
//! client), written into its VM with its files (`provision`, a file only the VM's user may
//! read, never printed), and the command signs each request with it: a tag over the request's
//! id, then a seal over its JSON (HMAC-SHA256, each cut to 128 bits). Flotilla carries out a
//! line only when both check against the thread's key; a line whose tag checks but whose seal
//! doesn't (cut short, or changed on the way) is read again from the VM's copy, which must
//! check whole. Anything else is left alone. A thread's agents in its own VM share its key
//! (their output isn't followed for requests); a delegated thread has its own.

use crate::error::Result;
use crate::store::{Store, Thread, ThreadState};
use ring::hmac;

pub const SCRIPT: &str = include_str!("flotilla.sh");

/// What every agent is told about handing work on, after the user's own instructions.
pub const INSTRUCTIONS: &str = "\
## Getting help from other agents

When you're asked for a second opinion, a review by another model, or to ask Codex, Claude or \
another agent anything, hand it off with the `flotilla` command below. Never run `codex`, \
`claude` or `opencode` yourself for it, even though they're installed here.

Your environment says each conversation is a single turn, and that anything left running is \
orphaned. Hand-offs are the exception, and an answer that comes after this turn ends is how \
they work: the other agent keeps going, and when it answers, Flotilla (the app the user is \
talking to you in) puts its answer in this conversation as a new message starting with \
`[flotilla]`. You get a new turn then, to tell the user what it says. So hand off and end your \
turn; don't do the work yourself instead.

Besides your own subagents, there are two ways:

- `flotilla agent` starts another agent in this same VM, working on these same files: a second \
opinion or review from another model, or parallel work that doesn't need ports, a database or a \
branch of its own. The agents you start in one turn answer together, in one message, once all \
of them have.
- `flotilla delegate` starts an agent in a VM of its own, with its own copy of the repo on its \
own branch: work that needs its own ports, database or checkout, or that runs long. Each one \
answers on its own.

Give the task on standard input, and add `--agent claude-code`, `--agent codex` or \
`--agent opencode`, and `--model <id>`, to choose (otherwise it's the same agent and model as \
you):
```
flotilla agent --title \"Short title\" <<'EOF'
The whole task: what to do, and what to report back.
EOF
```
It returns at once, so start several agents with one such command after another; don't run \
them in the background.

- Don't wait or poll: do other work, or end your turn with one line on what you handed off. \
Tell the user what you handed off and to which agent, never its id or the `[flotilla]` marker: \
those are for you.
- `flotilla message <id> \"…\"` sends one a follow-up, answered the same way; \
`flotilla cancel <id>` stops it.
- The other agent sees only the task you write, so make it stand alone. Ask a delegated one to \
commit and push its branch when you need its changes.
";

/// What every agent is told about asking you, after [`INSTRUCTIONS`]. (boat turns Claude Code's
/// and OpenCode's own ask tools off. Codex's works, but Codex seldom thinks to use it unasked,
/// and its question shows only once its record is read: so Codex is told this too, and its own
/// tool is still read when it uses it. See `crate::asks`.)
pub const ASK_INSTRUCTIONS: &str = "\
## Asking the user

When you need the user to choose or confirm something before you go on, ask with \
`flotilla ask`, not in your reply or with an ask tool of your own: they get your question with \
its options here, and on their phone.
```
flotilla ask \"Which database should I use?\" \"Postgres (recommended)\" \"SQLite\"
```
Give 2 to 4 options, the one you recommend first, and no \"Other\" (they can always write their \
own); add `--multi` when they may pick several, and see `flotilla ask --help` to ask up to 3 \
questions at once. Then end your turn, without asking it again in your reply: their answer \
comes as your next message. When another agent asked you for this, ask in your final message \
instead.
";

pub use flotilla_model::orchestrate::{
    CHILD_NOTE, Request, SIG_LEN, Seen, child_prompt, child_title, couldnt_start_text,
    parse_answer, parse_couldnt_start, parse_line, requests, task_of, unreadable_text, wake_text,
};

// ---- Which requests are the VM's own ----

/// Where a thread's key is in its VM, under the home folder.
pub const KEY_FILE: &str = ".flotilla/key";

fn key_key(thread: &str) -> String {
    format!("orch:key:{thread}")
}

/// The key the `flotilla` command in the thread's VM signs its requests with, once its VM
/// has been given one.
pub fn key(store: &Store, thread: &str) -> Option<String> {
    store
        .kv_get(&key_key(thread))
        .ok()
        .flatten()
        .filter(|k| k.len() == 64)
}

/// The thread's key, made now if it has none: 32 random bytes, in hex.
pub fn key_or_new(store: &Store, thread: &str) -> Result<String> {
    if let Some(k) = key(store, thread) {
        return Ok(k);
    }
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf)
        .map_err(|e| crate::error::Error::Invalid(format!("no randomness: {e}")))?;
    let k: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    store.kv_set(&key_key(thread), &k)?;
    Ok(k)
}

/// HMAC-SHA256 of `what` and `text` (the key is its 64 hex digits as text), cut to 128 bits,
/// in hex: what `flotilla.sh`'s `sign` makes.
fn mac(key: &str, what: &str, text: &str) -> String {
    let tag = hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA256, key.as_bytes()),
        format!("{what} {text}").as_bytes(),
    );
    tag.as_ref()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The signature the command makes for request `id` whose JSON is `json`.
pub fn sign(key: &str, id: &str, json: &str) -> String {
    format!("{}{}", mac(key, "id", id), mac(key, "request", json))
}

fn same(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// What a request line seen in a thread's tool output is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checked {
    /// The VM's own, whole: carry it out.
    Genuine(Request),
    /// The VM's own id, but not the whole request (cut short, or changed on the way): read its
    /// copy in the VM.
    ReadCopy(String),
    /// Not made by the thread's command (or from before requests were signed): left alone.
    Forged,
}

/// Check a line against the thread's key (`None`: its VM has none yet, so nothing it
/// shows is its own).
pub fn check(key: Option<&str>, seen: &Seen) -> Checked {
    let (Some(key), Some(sig)) = (key, seen.sig()) else {
        return Checked::Forged;
    };
    if sig.len() != SIG_LEN || !same(&sig[..SIG_LEN / 2], &mac(key, "id", seen.id())) {
        return Checked::Forged;
    }
    match seen {
        Seen::Whole { request, json, .. }
            if same(&sig[SIG_LEN / 2..], &mac(key, "request", json)) =>
        {
            Checked::Genuine(request.clone())
        }
        _ => Checked::ReadCopy(seen.id().to_string()),
    }
}

/// The command that reads a request's copy in the VM: its line as the command printed it.
/// (`id` is a checked UUID.)
pub fn read_copy_command(id: &str) -> String {
    format!("cat ~/.flotilla/requests/{id}.line")
}

/// A copy read back: the request, if it's request `id` and checks whole.
pub fn from_copy(key: &str, id: &str, text: &str) -> Option<Request> {
    let seen = parse_line(text.lines().next()?)?;
    match check(Some(key), &seen) {
        Checked::Genuine(r) if r.id() == id => Some(r),
        _ => None,
    }
}

fn handled_key(thread: &str, request: &str) -> String {
    format!("orch:req:{thread}:{request}")
}

/// What a thread's request led to (a delegate's child thread id), once it was carried out.
pub fn handled(store: &Store, thread: &str, request: &str) -> Result<Option<String>> {
    store.kv_get(&handled_key(thread, request))
}

pub fn set_handled(store: &Store, thread: &str, request: &str, outcome: &str) -> Result<()> {
    store.kv_set(&handled_key(thread, request), outcome)
}

/// Clear what requests from before they were kept by thread left (`orch:req:<id>`): each
/// thread's are cleared with it now.
pub fn forget_unowned_requests(store: &Store) -> Result<usize> {
    let prefix = "orch:req:";
    let old: Vec<String> = store
        .kv_with_prefix(prefix)?
        .into_iter()
        .map(|(k, _)| k)
        .filter(|k| !k[prefix.len()..].contains(':'))
        .collect();
    for k in &old {
        store.kv_delete(k)?;
    }
    Ok(old.len())
}

// ---- How much one thread may start ----

/// A thread this many hand-offs from one the user started may hand off no further.
pub const MAX_DEPTH: usize = 3;
/// What one turn may start or wake: delegates, agents, messages.
pub const MAX_PER_TURN: usize = 6;
/// A thread's own delegates with their VMs up, or its agents running in its VM, at once.
pub const MAX_LIVE: usize = 6;
/// Delegated threads with their VMs up under one thread the user started, however deep.
pub const MAX_TREE: usize = 12;

/// How many hand-offs the thread is from one the user started (0: the user's own).
pub fn depth(store: &Store, thread: &str) -> usize {
    let mut at = thread.to_string();
    let mut n = 0;
    // A loop of parents (never made, but kv is only text) stops somewhere.
    while let Some(p) = parent(store, &at) {
        n += 1;
        if n > 32 {
            break;
        }
        at = p;
    }
    n
}

/// The thread the user started that `thread`'s chain of hand-offs comes from.
fn root(parents: &std::collections::HashMap<String, String>, thread: &str) -> String {
    let mut at = thread;
    for _ in 0..32 {
        match parents.get(at) {
            Some(p) => at = p,
            None => break,
        }
    }
    at.to_string()
}

/// A thread's VM is up, or on its way.
pub fn vm_up(t: &Thread) -> bool {
    matches!(
        t.state,
        ThreadState::Creating | ThreadState::Starting | ThreadState::Running | ThreadState::Idle
    )
}

fn waking_key(child: &str) -> String {
    format!("orch:waking:{child}")
}

/// A message on its way to a delegated thread whose VM is stopped holds a place among the VMs
/// up until its VM is up (or for this long, if the message is lost).
const WAKING_MS: i64 = 3 * 60 * 1000;

/// Hold `child`'s place among the VMs up while a message wakes it.
pub fn reserve_wake(store: &Store, child: &str) -> Result<()> {
    store.kv_set(&waking_key(child), &crate::util::now_ms().to_string())
}

/// The message went, or didn't: its place is free (once up, the VM counts as itself).
pub fn release_wake(store: &Store, child: &str) -> Result<()> {
    store.kv_delete(&waking_key(child))
}

/// A message waking `child` holds its place now.
pub fn reserved(store: &Store, child: &str) -> bool {
    waking(store).contains(child)
}

fn waking(store: &Store) -> std::collections::HashSet<String> {
    let prefix = waking_key("");
    let now = crate::util::now_ms();
    store
        .kv_with_prefix(&prefix)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, v)| v.parse::<i64>().is_ok_and(|at| now - at < WAKING_MS))
        .map(|(k, _)| k[prefix.len()..].to_string())
        .collect()
}

/// Delegated threads whose VMs are up (or a message is waking): `thread`'s own, and all under
/// the thread the user started that it comes from.
fn live_delegates(store: &Store, thread: &str) -> (usize, usize) {
    let parents = parents(store);
    let threads = store.threads().unwrap_or_default();
    let waking = waking(store);
    let up: std::collections::HashSet<&str> = threads
        .iter()
        .filter(|t| vm_up(t) || waking.contains(&t.id))
        .map(|t| t.id.as_str())
        .collect();
    let own = parents
        .iter()
        .filter(|(c, p)| p.as_str() == thread && up.contains(c.as_str()))
        .count();
    let top = root(&parents, thread);
    let tree = parents
        .keys()
        .filter(|c| up.contains(c.as_str()) && root(&parents, c) == top)
        .count();
    (own, tree)
}

fn turn_key(thread: &str) -> String {
    format!("orch:turn:{thread}")
}

/// A turn's count is kept this long after its last request (ms): long past any request of it
/// still to read back (cut short, a few a poll).
const TURN_KEPT_MS: i64 = 24 * 60 * 60 * 1000;

/// Count a request toward its turn (`batch`): how many came before it in that turn.
fn count_in_turn(store: &Store, thread: &str, batch: &str) -> usize {
    let now = crate::util::now_ms();
    let mut turns: Vec<(String, usize, i64)> = store
        .kv_get(&turn_key(thread))
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default();
    let n = turns
        .iter()
        .find(|(b, _, _)| b == batch)
        .map_or(0, |(_, n, _)| *n);
    turns.retain(|(b, _, at)| b != batch && now - at < TURN_KEPT_MS);
    turns.push((batch.to_string(), n + 1, now));
    if let Ok(v) = serde_json::to_string(&turns) {
        let _ = store.kv_set(&turn_key(thread), &v);
    }
    n
}

/// What one turn may ask the user (`flotilla ask`): any more go unasked, so a runaway turn
/// can't fill the thread with questions, or the phone with pushes. (Not told: telling it starts
/// a turn, which would pass the questions it did ask.)
pub const MAX_ASKS_PER_TURN: usize = 3;

/// Count a question toward its turn (`batch`): whether it's within what one turn may ask.
pub fn ask_this_turn(store: &Store, thread: &str, batch: &str) -> bool {
    count_in_turn(store, thread, &format!("ask:{batch}")) < MAX_ASKS_PER_TURN
}

/// What one turn may stop (`flotilla cancel`): every delegate and agent it may have going at
/// once. Any more are let be, unsaid, as asks over theirs are: each costs a boat call, and
/// telling would start a turn.
pub const MAX_CANCELS_PER_TURN: usize = 2 * MAX_LIVE;

/// Count a cancel toward its turn (`batch`): whether it's within what one turn may stop. Its
/// own count, apart from what the turn starts.
pub fn cancel_this_turn(store: &Store, thread: &str, batch: &str) -> bool {
    count_in_turn(store, thread, &format!("cancel:{batch}")) < MAX_CANCELS_PER_TURN
}

/// What a request asks to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// A thread with a VM of its own.
    Delegate,
    /// An agent in the thread's VM, or a follow-up for one.
    Agent,
    /// A follow-up for a delegated thread: `wakes` if its VM isn't up.
    Message { wakes: bool },
}

/// A request over a limit: why, for the agent; and whether to tell it (only the first in a
/// turn over the turn's limit is told, so a runaway turn can't flood its own conversation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub why: String,
    pub tell: bool,
}

/// Count a request that starts or wakes something toward its turn (`batch`): refused past the
/// turn's limit. Counted before anything else refuses it, so every refusal a turn is told of
/// counts too.
pub fn this_turn(store: &Store, thread: &str, batch: &str) -> std::result::Result<(), Refused> {
    let before = count_in_turn(store, thread, batch);
    if before >= MAX_PER_TURN {
        return Err(Refused {
            why: format!(
                "one turn can hand off at most {MAX_PER_TURN} tasks, so this one and any more this turn didn't start: do it yourself, or hand it off in a later turn"
            ),
            tell: before == MAX_PER_TURN,
        });
    }
    Ok(())
}

/// What one tree of threads (the user's and every thread handed work from it) may hand off
/// since the user last wrote in it: delegates, agents in a VM (and follow-ups for them), and
/// messages that wake a delegate's VM. Each answer is a new turn, so the limits on a turn and on
/// what's up at once don't bound the total: an agent re-delegating on every answer would run
/// all night. This does.
pub const MAX_SINCE_YOU: usize = 20;

/// Why a hand-off over [`MAX_SINCE_YOU`] didn't start, for the agent and the user both.
pub fn over_budget_why() -> String {
    format!(
        "Hand-off limit reached ({MAX_SINCE_YOU} since your last message). Send a message to continue."
    )
}

fn budget_key(root: &str) -> String {
    format!("orch:budget:{root}")
}

/// A tree's hand-offs since the user last wrote in it, and whether it was told it's at the limit.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Budget {
    n: usize,
    told: bool,
}

/// Whether what a request starts counts toward its tree's [`MAX_SINCE_YOU`]: all but a message
/// to a delegate whose VM is up.
pub fn spends(start: Start) -> bool {
    start != Start::Message { wakes: false }
}

/// Count a hand-off from `thread` toward its tree's [`MAX_SINCE_YOU`]: refused once the tree
/// has handed off that many since the user last wrote in it, told the first time only (telling
/// starts a turn).
pub fn spend(store: &Store, thread: &str) -> std::result::Result<(), Refused> {
    let key = budget_key(&root(&parents(store), thread));
    let spent = (|| -> Result<std::result::Result<(), Refused>> {
        let tx = store.write()?;
        let mut b: Budget = store
            .kv_get(&key)?
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default();
        let out = if b.n >= MAX_SINCE_YOU {
            let tell = !b.told;
            b.told = true;
            Err(Refused {
                why: over_budget_why(),
                tell,
            })
        } else {
            b.n += 1;
            Ok(())
        };
        store.kv_set(&key, &serde_json::to_string(&b)?)?;
        tx.commit()?;
        Ok(out)
    })();
    // Not counted (the database is busy): it goes, as it would have before there was a limit.
    spent.unwrap_or(Ok(()))
}

/// The user wrote in `thread` (a prompt or a steer of their own): its tree may hand off
/// [`MAX_SINCE_YOU`] again.
pub fn user_wrote(store: &Store, thread: &str) -> Result<()> {
    store.kv_delete(&budget_key(&root(&parents(store), thread)))
}

/// Whether thread `thread` has room for what a request asks to start: not too deep, and not
/// too many VMs up or agents running.
pub fn room(store: &Store, thread: &str, start: Start) -> std::result::Result<(), Refused> {
    let refuse = |why: String| Err(Refused { why, tell: true });
    if start == Start::Delegate && depth(store, thread) >= MAX_DEPTH {
        return refuse(format!(
            "hand-offs go at most {MAX_DEPTH} deep, and this thread is {MAX_DEPTH} down from the user's: do it yourself"
        ));
    }
    match start {
        Start::Agent => {
            let running = crate::side::of(store, thread)
                .iter()
                .filter(|s| s.running())
                .count();
            if running >= MAX_LIVE {
                return refuse(format!(
                    "{running} agents are running in this VM, the most at once: wait for their answers"
                ));
            }
        }
        Start::Delegate | Start::Message { wakes: true } => {
            let (own, tree) = live_delegates(store, thread);
            if own >= MAX_LIVE {
                return refuse(format!(
                    "{own} threads you handed work to have their VMs up, the most at once: wait for their answers"
                ));
            }
            if tree >= MAX_TREE {
                return refuse(format!(
                    "{tree} threads handed work from the user's thread have their VMs up, the most at once: wait for their answers"
                ));
            }
        }
        Start::Message { wakes: false } => {}
    }
    Ok(())
}

// ---- Messages Flotilla sends a thread of its own accord ----

fn held_key(thread: &str) -> String {
    format!("orch:held:{thread}:")
}

/// Whether a thread may be sent something it didn't ask for now (its agents' report) without
/// undoing the user's Stop or Archive: its VM is up and it isn't archived. A send to it
/// otherwise starts the VM again and brings it back. (Its agents ran in that VM: their report
/// doesn't start it again. A child's answer may: [`may_wake`].)
pub fn awake(store: &Store, thread: &str) -> bool {
    let up = store
        .thread(thread)
        .ok()
        .flatten()
        .is_some_and(|t| matches!(t.state, ThreadState::Idle | ThreadState::Running));
    up && !store.is_archived(thread).unwrap_or(true)
}

/// Whether a message the user didn't send (Flotilla's own, like a hand-off's answer, or an
/// agent's to another) may go to `thread` now. A send starts a stopped VM and brings an archived
/// thread back, so not while the user has it archived, or has stopped its VM and not started it
/// again since (however it looks while it stops); a VM that stopped by itself, idle, is
/// started.
pub fn may_wake(store: &Store, thread: &str) -> bool {
    let Ok(Some(t)) = store.thread(thread) else {
        return false;
    };
    t.state != ThreadState::Expired
        && !store.is_archived(thread).unwrap_or(true)
        && !crate::lifecycle::stopped_by_user(store, thread)
}

/// The count every kept message's id comes from, so they keep their order (whatever the
/// clock does, and across restarts).
const HELD_COUNT: &str = "orch:held_count";

/// A message kept for a thread.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Kept {
    /// Its place in the thread's queue.
    #[serde(skip)]
    pub id: String,
    pub text: String,
    /// An agent's follow-up for a thread it delegated to: it waits for the answer from when
    /// the message reaches it (not before, so an earlier answer doesn't stand for it).
    #[serde(default)]
    pub awaited: bool,
    /// Counted toward its tree's hand-offs ([`spend`]): when it was asked for, to wake the VM,
    /// or once a VM that was up then has stopped and it wakes it.
    #[serde(default)]
    pub charged: bool,
}

/// Keep a message for `thread`, to go when [`may_wake`] says it may: every message Flotilla
/// sends of its own accord goes this way, oldest first, one at a time, and is kept until it has
/// reached the thread. Each is a row of its own, so keeping one and sending another never
/// overwrite each other.
pub fn hold(store: &Store, thread: &str, text: &str) -> Result<()> {
    hold_with(store, thread, text, Vec::new())
}

/// [`hold`] for a parent's follow-up for `child`: the parent waits for its answer once it's
/// sent. `charged`: counted toward its tree's hand-offs already (it was to wake the VM).
pub fn hold_follow_up(store: &Store, child: &str, text: &str, charged: bool) -> Result<()> {
    keep(
        store,
        child,
        &Kept {
            id: String::new(),
            text: text.into(),
            awaited: true,
            charged,
        },
        &[],
    )
}

/// The follow-up kept for `thread` as `id` has been counted toward its tree's hand-offs, so a
/// send tried again doesn't count it again.
pub fn charged(store: &Store, thread: &str, id: &str) -> Result<()> {
    let key = format!("{}{id}", held_key(thread));
    let Some(mut kept) = store
        .kv_get(&key)?
        .and_then(|v| serde_json::from_str::<Kept>(&v).ok())
    else {
        return Ok(());
    };
    kept.charged = true;
    store.kv_set(&key, &serde_json::to_string(&kept)?)
}

/// [`hold`], writing `also` (`kv` entries) in the same write: what the message reports counts
/// as reported once, and only once, it's kept.
pub fn hold_with(
    store: &Store,
    thread: &str,
    text: &str,
    also: Vec<(String, String)>,
) -> Result<()> {
    keep(
        store,
        thread,
        &Kept {
            id: String::new(),
            text: text.into(),
            awaited: false,
            charged: false,
        },
        &also,
    )
}

fn keep(store: &Store, thread: &str, kept: &Kept, also: &[(String, String)]) -> Result<()> {
    store
        .kv_append(
            HELD_COUNT,
            &held_key(thread),
            &serde_json::to_string(kept)?,
            also,
        )
        .map(|_| ())
}

/// The messages kept for `thread`, oldest first.
pub fn held(store: &Store, thread: &str) -> Vec<Kept> {
    let prefix = held_key(thread);
    let mut out: Vec<Kept> = store
        .kv_with_prefix(&prefix)
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| {
            let id = k[prefix.len()..].to_string();
            match serde_json::from_str::<Kept>(&v) {
                Ok(kept) => Kept { id, ..kept },
                Err(_) => Kept {
                    id,
                    text: v,
                    awaited: false,
                    charged: false,
                },
            }
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// The threads with messages kept for them.
pub fn holding(store: &Store) -> Vec<String> {
    let prefix = "orch:held:";
    let mut out: Vec<String> = store
        .kv_with_prefix(prefix)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, _)| {
            k[prefix.len()..]
                .split_once(':')
                .map(|(t, _)| t.to_string())
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Message `id` reached `thread`: it's kept no longer.
pub fn delivered(store: &Store, thread: &str, id: &str) -> Result<()> {
    store.kv_delete(&format!("{}{id}", held_key(thread)))
}

pub fn clear_held(store: &Store, thread: &str) -> Result<()> {
    for kept in held(store, thread) {
        delivered(store, thread, &kept.id)?;
    }
    Ok(())
}

/// Every key a thread's hand-offs keep that goes with it: its key, its turns' counts, a wake
/// on its way; and the prefixes of its requests' outcomes and the messages kept for it.
/// (`Store::delete_thread` clears them.)
pub fn thread_keys(thread: &str) -> (Vec<String>, Vec<String>) {
    (
        vec![key_key(thread), turn_key(thread), waking_key(thread)],
        vec![handled_key(thread, ""), held_key(thread)],
    )
}

fn since_key(thread: &str) -> String {
    format!("orch:since:{thread}")
}

/// This database has had the thread from its start: every request in it is new to it.
pub fn seen_from_start(store: &Store, thread: &str) -> Result<()> {
    store.kv_set(&since_key(thread), "0")
}

/// Requests in a thread older than this (ms) were made before this database first saw it (a
/// rebuilt database), so they may have been carried out already and are left alone. Requests
/// made while Flotilla was closed are newer, and are carried out when it reopens. `latest` is
/// the newest message time in its history as read now (boat's clock, as the requests'); with
/// no history yet, `None`.
pub fn requests_since(store: &Store, thread: &str, latest: Option<i64>) -> Option<i64> {
    if let Some(ms) = store
        .kv_get(&since_key(thread))
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
    {
        return Some(ms);
    }
    let since = latest? + 1;
    let _ = store.kv_set(&since_key(thread), &since.to_string());
    Some(since)
}

fn parent_key(child: &str) -> String {
    format!("parent:{child}")
}

/// The thread that delegated this one, if one did.
pub fn parent(store: &Store, child: &str) -> Option<String> {
    store
        .kv_get(&parent_key(child))
        .ok()
        .flatten()
        .filter(|p| !p.is_empty())
}

pub fn set_parent(store: &Store, child: &str, parent: &str) -> Result<()> {
    store.kv_set(&parent_key(child), parent)
}

/// Every child's parent, child id → parent id.
pub fn parents(store: &Store) -> std::collections::HashMap<String, String> {
    let prefix = parent_key("");
    store
        .kv_with_prefix(&prefix)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k[prefix.len()..].to_string(), v))
        .collect()
}

fn awaiting_key(child: &str) -> String {
    format!("orch:awaiting:{child}")
}

/// One of a parent's waits for an answer from its child: what the child had when it began (its
/// messages' ids, so only one sent after counts), under a number of its own. A parent may wait
/// on several at once (a follow-up while the last is answered): each is begun, met and given up
/// on by itself, in one write each, so none undoes another.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Wait {
    n: u64,
    known: Vec<String>,
}

/// The count every wait's number comes from.
const WAIT_COUNT: &str = "orch:wait_count";

fn waits_of(raw: Option<String>) -> Vec<Wait> {
    let raw = raw.unwrap_or_default();
    if raw.is_empty() {
        return Vec::new();
    }
    // One wait, as they were kept before.
    if let Ok(known) = serde_json::from_str::<Vec<String>>(&raw) {
        return vec![Wait { n: 0, known }];
    }
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Change `child`'s waits in one write (the write lock held from the read).
fn change_waits<T>(
    store: &Store,
    child: &str,
    f: impl FnOnce(&Store, &mut Vec<Wait>) -> Result<T>,
) -> Result<T> {
    let tx = store.write()?;
    let mut waits = waits_of(store.kv_get(&awaiting_key(child))?);
    let out = f(store, &mut waits)?;
    let value = if waits.is_empty() {
        String::new()
    } else {
        serde_json::to_string(&waits)?
    };
    store.kv_set(&awaiting_key(child), &value)?;
    tx.commit()?;
    Ok(out)
}

/// The parent asked the child for something (or, `false`, waits for nothing more): its answer
/// goes back once a message sent after now has had its turn. Called before the send.
pub fn set_awaiting(store: &Store, child: &str, awaiting: bool) -> Result<()> {
    if awaiting {
        begin_wait(store, child).map(|_| ())
    } else {
        change_waits(store, child, |_, waits| {
            waits.clear();
            Ok(())
        })
    }
}

/// Begin a wait for an answer to a message about to go to `child`: its number, to give it up
/// by ([`end_wait`]) if the message doesn't go.
pub fn begin_wait(store: &Store, child: &str) -> Result<u64> {
    change_waits(store, child, |store, waits| {
        let n = store
            .kv_get(WAIT_COUNT)?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            + 1;
        store.kv_set(WAIT_COUNT, &n.to_string())?;
        let known = store
            .sent_prompts(child)?
            .into_iter()
            .map(|s| s.id)
            .collect();
        waits.push(Wait { n, known });
        Ok(n)
    })
}

/// Wait `n` for `child`'s answer is given up on (its message didn't go); the others stay.
pub fn end_wait(store: &Store, child: &str, n: u64) -> Result<()> {
    change_waits(store, child, |_, waits| {
        waits.retain(|w| w.n != n);
        Ok(())
    })
}

/// The child's answer is in for what its parent waits on: whether any wait is met now, each
/// met one no longer waiting (those begun since, for a message still to have its turn, stay).
pub fn answer_waits(store: &Store, child: &str) -> Result<bool> {
    change_waits(store, child, |store, waits| {
        let before = waits.len();
        let mut kept = Vec::new();
        for w in waits.drain(..) {
            if !answered(store, child, &w.known)? {
                kept.push(w);
            }
        }
        *waits = kept;
        Ok(waits.len() < before)
    })
}

/// The children a parent waits on now.
pub fn waited_on(store: &Store) -> Vec<String> {
    let prefix = awaiting_key("");
    store
        .kv_with_prefix(&prefix)
        .unwrap_or_default()
        .into_iter()
        .map(|(k, _)| k[prefix.len()..].to_string())
        .collect()
}

/// While the parent waits: what the child had when its oldest wait began.
pub fn awaiting(store: &Store, child: &str) -> Option<Vec<String>> {
    waits_of(store.kv_get(&awaiting_key(child)).ok().flatten())
        .into_iter()
        .next()
        .map(|w| w.known)
}

/// The threads waiting on an answer from a child (`parents`: child → parent).
pub fn waiting_parents(
    store: &Store,
    parents: &std::collections::HashMap<String, String>,
) -> std::collections::HashSet<String> {
    let prefix = awaiting_key("");
    store
        .kv_with_prefix(&prefix)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .filter_map(|(k, _)| parents.get(&k[prefix.len()..]).cloned())
        .collect()
}

/// Whether the child's answer is in: a message it didn't have (`known`) when its parent asked
/// reached boat (not a placeholder still sending, or one boat never got), its turn (a prompt's
/// own, or the one a steer joined) has run, and no turn of it is still going.
pub fn answered(store: &Store, child: &str, known: &[String]) -> Result<bool> {
    if !store.unsettled_turns(child)?.is_empty() {
        return Ok(false);
    }
    let turns = store.turns(child)?;
    Ok(store.sent_prompts(child)?.iter().any(|s| {
        !known.contains(&s.id)
            && !s.id.starts_with("local-")
            && !s.id.starts_with(crate::lifecycle::UNSENT_PREFIX)
            && (s.kind == "steer" || turns.iter().any(|t| t.prompt_id == s.id))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Thread, ThreadState};
    use std::process::{Command, Stdio};

    fn tool(text: &str) -> flotilla_proto::Message {
        flotilla_proto::Message {
            kind: "tool".into(),
            tool_result_json: Some(
                serde_json::to_string(&serde_json::Value::String(text.into())).unwrap(),
            ),
            ..Default::default()
        }
    }

    /// Run the `flotilla` command as an agent in a VM does, in `home`: its output and error.
    fn run(home: &std::path::Path, args: &[&str], stdin: &str) -> (bool, String, String) {
        let mut child = Command::new("bash")
            .arg("-c")
            .arg(SCRIPT)
            .arg("flotilla")
            .args(args)
            .env("HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into(),
            String::from_utf8_lossy(&out.stderr).into(),
        )
    }

    /// The command as it runs in a VM, with the key provisioning put there: what it prints
    /// checks against the thread's key, and nothing it prints says the key. The same line under
    /// another thread's key, unsigned, or with a signature made up doesn't; changed on the way,
    /// it's read again from its copy, which checks whole.
    #[test]
    fn what_the_command_signs_checks_and_nothing_else_does() {
        if Command::new("jq").arg("--version").output().is_err() {
            eprintln!("skipped: no jq here");
            return;
        }
        let home = std::env::temp_dir().join(format!("flotilla-sign-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join(".flotilla")).unwrap();
        let store = Store::open_in_memory().unwrap();
        // Before the VM has a key, the command says so and prints no request.
        let (ok, out, err) = run(&home, &["delegate", "--title", "Add tests"], "Write tests.");
        assert!(
            !ok && !out.contains("FLOTILLA") && err.contains("isn't set up"),
            "{out}{err}"
        );
        let key = key_or_new(&store, "t1").unwrap();
        assert_eq!(
            key_or_new(&store, "t1").unwrap(),
            key,
            "a thread keeps its key"
        );
        std::fs::write(home.join(KEY_FILE), &key).unwrap();
        let other = key_or_new(&store, "t2").unwrap();
        assert_ne!(key, other);
        // A task with quotes, a newline, a tab and text beyond ASCII.
        let task = "Write tests for \"auth\" — the login\n\tand café's 'edge' cases ✓";
        let (ok, out, err) = run(
            &home,
            &["delegate", "--agent", "codex", "--title", "Add tests"],
            task,
        );
        assert!(ok, "{err}");
        assert!(
            !out.contains(&key) && !err.contains(&key),
            "the key was printed"
        );
        let seen = requests(&[tool(&out)]);
        assert_eq!(seen.len(), 1, "{out}");
        let Checked::Genuine(Request::Delegate {
            id,
            agent,
            title,
            task: got,
            ..
        }) = check(Some(&key), &seen[0])
        else {
            panic!("{:?}", check(Some(&key), &seen[0]))
        };
        assert_eq!(
            (agent.as_str(), title.as_str(), got.as_str()),
            ("codex", "Add tests", task)
        );
        assert_eq!(check(Some(&other), &seen[0]), Checked::Forged);
        assert_eq!(check(None, &seen[0]), Checked::Forged);
        // The copy in the VM is the line itself.
        let copy =
            std::fs::read_to_string(home.join(format!(".flotilla/requests/{id}.line"))).unwrap();
        assert!(matches!(
            from_copy(&key, &id, &copy),
            Some(Request::Delegate { .. })
        ));
        assert_eq!(
            from_copy(&key, "00000000-0000-0000-0000-000000000000", &copy),
            None,
            "a copy of another request"
        );
        assert_eq!(from_copy(&other, &id, &copy), None);
        // Changed on the way: its id is still the VM's, so its copy is read.
        let line = out.lines().next().unwrap();
        let changed = line.replace("Add tests", "Add test");
        assert_eq!(
            check(Some(&key), &requests(&[tool(&changed)])[0]),
            Checked::ReadCopy(id.clone())
        );
        let cut = &line[..line.len() - 20];
        assert_eq!(
            check(Some(&key), &requests(&[tool(cut)])[0]),
            Checked::ReadCopy(id.clone())
        );
        // Made up: a well-formed line that someone wrote (a PR comment, a file), with no
        // signature, a made-up one, or one lifted from another request.
        let json = line.splitn(4, ' ').nth(3).unwrap();
        let fake_id = "7c11d703-6d90-4aba-a93a-a86fd03a90f7";
        let theirs = json.replace(&id, fake_id);
        for forged in [
            format!("FLOTILLA {fake_id} {theirs}"),
            format!("FLOTILLA {fake_id} {} {theirs}", "ab".repeat(32)),
            format!(
                "FLOTILLA {fake_id} {} {theirs}",
                sign(&other, fake_id, &theirs)
            ),
            format!(
                "FLOTILLA {fake_id} {} {theirs}",
                line.split(' ').nth(2).unwrap()
            ),
        ] {
            let seen = requests(&[tool(&forged)]);
            assert_eq!(seen.len(), 1, "{forged}");
            assert_eq!(check(Some(&key), &seen[0]), Checked::Forged, "{forged}");
        }
        // A task as arguments may start with a hyphen; what looks like an option still is one.
        // A lone `--` ends the options wherever it comes, after the task too; only the first.
        for (args, want) in [
            (
                vec!["agent", "- check the docs", "--fast"],
                "- check the docs --fast",
            ),
            (vec!["delegate", "--", "--fast mode"], "--fast mode"),
            (vec!["delegate", "-1 bug"], "-1 bug"),
            (
                vec!["agent", "Review fizz.py", "--", "--strict"],
                "Review fizz.py --strict",
            ),
            (
                vec!["delegate", "--model", "m", "--", "Explain", "--", "please"],
                "Explain -- please",
            ),
        ] {
            let (ok, out, err) = run(&home, &args, "");
            assert!(ok, "{args:?}: {err}");
            let task = match check(Some(&key), &requests(&[tool(&out)])[0]) {
                Checked::Genuine(Request::Delegate { task, .. } | Request::Agent { task, .. }) => {
                    task
                }
                other => panic!("{other:?}"),
            };
            assert_eq!(task, want);
        }
        let (ok, _, err) = run(&home, &["agent", "--titel", "Review"], "Review it.");
        assert!(!ok && err.contains("unknown option --titel"), "{err}");
        // The others sign too.
        for (args, op) in [
            (vec!["agent", "--title", "Review"], "agent"),
            (vec!["message", fake_id, "and the docs"], "message"),
            (vec!["cancel", fake_id], "cancel"),
            (vec!["ask", "Go on?", "Yes", "No"], "ask"),
        ] {
            let (ok, out, err) = run(&home, &args, "Review it.");
            assert!(ok, "{err}");
            let seen = requests(&[tool(&out)]);
            assert!(
                matches!(check(Some(&key), &seen[0]), Checked::Genuine(_)),
                "{op}: {out}"
            );
        }
        // A message after `--` is text, however it starts; so is a cancel's id.
        for args in [
            vec!["message", fake_id, "--", "--fix it"],
            vec!["message", "--", fake_id, "--fix it"],
        ] {
            let (ok, out, err) = run(&home, &args, "");
            assert!(ok, "{args:?}: {err}");
            let Checked::Genuine(Request::Message { to, text, .. }) =
                check(Some(&key), &requests(&[tool(&out)])[0])
            else {
                panic!("{out}")
            };
            assert_eq!(
                (to.as_str(), text.as_str()),
                (fake_id, "--fix it"),
                "{args:?}"
            );
        }
        let (ok, out, err) = run(&home, &["cancel", "--", fake_id], "");
        assert!(
            ok && matches!(
                check(Some(&key), &requests(&[tool(&out)])[0]),
                Checked::Genuine(Request::Cancel { .. })
            ),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `flotilla ask` as an agent runs it: one question as arguments, or up to three as JSON on
    /// standard input, in Codex's or Claude Code's shape or close to it, tidied to what's kept
    /// and signed. What the limits refuse it says, printing no request; and its limits are
    /// `asks::checked`'s.
    #[test]
    fn the_ask_command_reads_questions_and_holds_to_the_limits() {
        if Command::new("jq").arg("--version").output().is_err() {
            eprintln!("skipped: no jq here");
            return;
        }
        use crate::asks;
        use flotilla_proto::ask::{Choice, Question};
        let home = std::env::temp_dir().join(format!("flotilla-ask-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(home.join(".flotilla")).unwrap();
        let store = Store::open_in_memory().unwrap();
        let key = key_or_new(&store, "t1").unwrap();
        std::fs::write(home.join(KEY_FILE), &key).unwrap();
        let asked = |args: &[&str], stdin: &str| -> Vec<Question> {
            let (ok, out, err) = run(&home, args, stdin);
            assert!(ok, "{args:?} {stdin}: {err}");
            assert!(
                out.contains("End your turn now") && !out.contains(&key),
                "{out}"
            );
            let seen = requests(&[tool(&out)]);
            let Checked::Genuine(Request::Ask { questions, .. }) = check(Some(&key), &seen[0])
            else {
                panic!("{out}")
            };
            // What the command lets through, Flotilla keeps as it is.
            assert_eq!(asks::checked(questions.clone()), Ok(questions.clone()));
            questions
        };
        let choice = |label: &str| Choice {
            label: label.into(),
            description: None,
        };
        assert_eq!(
            asked(
                &[
                    "ask",
                    "Which file name should I create?",
                    "alpha.txt (recommended)",
                    "beta.txt"
                ],
                ""
            ),
            [Question {
                header: None,
                question: "Which file name should I create?".into(),
                options: vec![choice("alpha.txt (recommended)"), choice("beta.txt")],
                multi: false
            }]
        );
        assert_eq!(
            asked(
                &[
                    "ask",
                    "--multi",
                    "--header",
                    "Checks",
                    "Which checks?",
                    "lint",
                    "test",
                    "docs"
                ],
                ""
            )[0],
            Question {
                header: Some("Checks".into()),
                question: "Which checks?".into(),
                options: vec![choice("lint"), choice("test"), choice("docs")],
                multi: true
            }
        );
        // Options and questions that look like flags are text: after the question, and a question
        // starting with a hyphen (after `--`, or one an option couldn't be).
        let plain = |question: &str, options: &[&str]| Question {
            header: None,
            question: question.into(),
            options: options.iter().map(|o| choice(o)).collect(),
            multi: false,
        };
        for (args, want) in [
            (
                vec!["ask", "Which flag?", "--safe", "--fast"],
                plain("Which flag?", &["--safe", "--fast"]),
            ),
            (
                vec!["ask", "Which?", "--help", "-h", "--version"],
                plain("Which?", &["--help", "-h", "--version"]),
            ),
            (
                vec!["ask", "--", "-Question?", "yes", "no"],
                plain("-Question?", &["yes", "no"]),
            ),
            (
                vec!["ask", "--", "--safe", "yes", "no"],
                plain("--safe", &["yes", "no"]),
            ),
            (
                vec!["ask", "-v or -q?", "-v", "-q"],
                plain("-v or -q?", &["-v", "-q"]),
            ),
            (
                vec!["ask", "-1 or +1?", "-1", "+1"],
                plain("-1 or +1?", &["-1", "+1"]),
            ),
            // A `--` after the question ends the options too (as an agent writes it); only the
            // first is dropped, so one after it is an option.
            (
                vec!["ask", "Which mode?", "--", "--safe", "--fast"],
                plain("Which mode?", &["--safe", "--fast"]),
            ),
            (
                vec![
                    "ask",
                    "--header",
                    "Mode",
                    "Which mode?",
                    "--safe",
                    "--fast",
                    "--",
                ],
                Question {
                    header: Some("Mode".into()),
                    ..plain("Which mode?", &["--safe", "--fast"])
                },
            ),
            (
                vec!["ask", "--", "Which?", "--", "--fast"],
                plain("Which?", &["--", "--fast"]),
            ),
        ] {
            assert_eq!(asked(&args, ""), [want], "{args:?}");
        }
        // Claude Code's shape (multiSelect, options with descriptions), and a bare question;
        // labels made one line, the question's own lines kept.
        let claude = r#"{"questions":[{"header":"Name","question":"Which name?\nPick one.","options":[{"label":"alpha  (recommended)","description":"the\nfirst"},{"label":"beta"}],"multiSelect":true},{"question":"Go on?","options":["Yes","No"]}]}"#;
        let qs = asked(&["ask"], claude);
        assert_eq!(qs.len(), 2);
        assert_eq!(
            (
                qs[0].question.as_str(),
                qs[0].multi,
                qs[0].header.as_deref()
            ),
            ("Which name?\nPick one.", true, Some("Name"))
        );
        assert_eq!(
            qs[0].options[0],
            Choice {
                label: "alpha (recommended)".into(),
                description: Some("the first".into())
            }
        );
        assert_eq!(
            asked(&["ask"], r#"[{"title":"Go on?","options":["Yes","No"]}]"#)[0].question,
            "Go on?"
        );
        assert_eq!(
            asked(&["ask"], r#"{"question":"Go on?","options":["Yes","No"]}"#)[0].question,
            "Go on?"
        );
        // Refused, saying why, with nothing printed for Flotilla.
        let q = |n: usize| format!(r#"{{"question":"Q{n}?","options":["a","b"]}}"#);
        let long = "x".repeat(asks::MAX_QUESTION + 1);
        for (args, stdin, why) in [
            (
                vec!["ask", "Only one?", "a"],
                String::new(),
                "needs 2 to 4 options, not 1",
            ),
            (
                vec!["ask", "Five?", "a", "b", "c", "d", "e"],
                String::new(),
                "not 5",
            ),
            (
                vec!["ask"],
                format!(
                    r#"{{"questions":[{}]}}"#,
                    (1..=4).map(q).collect::<Vec<_>>().join(",")
                ),
                "1 to 3 questions at once, not 4",
            ),
            (vec!["ask"], r#"{"questions":[]}"#.into(), "not 0"),
            (
                vec!["ask"],
                r#"[{"question":"A?","options":["x"," x "]}]"#.into(),
                "the option \"x\" twice",
            ),
            (
                vec!["ask"],
                r#"[{"question":"  ","options":["x","y"]}]"#.into(),
                "question 1 has no text",
            ),
            (
                vec!["ask"],
                format!(r#"[{{"question":"{long}","options":["x","y"]}}]"#),
                "over 1000 characters",
            ),
            (
                vec!["ask"],
                format!(
                    r#"[{{"question":"A?","header":"{}","options":["x","y"]}}]"#,
                    "h".repeat(asks::MAX_HEADER + 1)
                ),
                "header of question 1",
            ),
            (
                vec!["ask"],
                format!(
                    r#"[{{"question":"A?","options":["{}","y"]}}]"#,
                    "l".repeat(asks::MAX_LABEL + 1)
                ),
                "over 120 characters",
            ),
            (
                vec!["ask"],
                format!(
                    r#"[{{"question":"A?","options":[{{"label":"x","description":"{}"}},"y"]}}]"#,
                    "d".repeat(asks::MAX_DESCRIPTION + 1)
                ),
                "description of option",
            ),
            (
                vec!["ask"],
                r#"[{"question":"A?","options":["","y"]}]"#.into(),
                "has no label",
            ),
            (vec!["ask"], "Which one, a or b?".into(), "isn't JSON"),
            (vec!["ask"], String::new(), "no question given"),
            (
                vec!["ask", "--mutli", "Which?", "a", "b"],
                String::new(),
                "unknown option --mutli",
            ),
            (
                vec!["ask", "-x", "Which?", "a", "b"],
                String::new(),
                "unknown option -x",
            ),
        ] {
            let (ok, out, err) = run(&home, &args, &stdin);
            assert!(
                !ok && !out.contains("FLOTILLA") && err.contains(why),
                "{args:?} {stdin}: {out} {err}"
            );
        }
        // The command's limits are Flotilla's.
        for (name, n) in [
            ("MAX_QUESTIONS", asks::MAX_QUESTIONS),
            ("MIN_OPTIONS", asks::MIN_OPTIONS),
            ("MAX_OPTIONS", asks::MAX_OPTIONS),
            ("MAX_QUESTION", asks::MAX_QUESTION),
            ("MAX_HEADER", asks::MAX_HEADER),
            ("MAX_LABEL", asks::MAX_LABEL),
            ("MAX_DESCRIPTION", asks::MAX_DESCRIPTION),
        ] {
            assert!(
                SCRIPT
                    .lines()
                    .any(|l| l.split_whitespace().any(|w| w == format!("{name}={n}"))),
                "{name}"
            );
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    fn thread(id: &str, state: ThreadState) -> Thread {
        Thread {
            id: id.into(),
            environment_id: None,
            environment_name: None,
            title: id.into(),
            sandbox_id: Some(format!("bx_{id}")),
            conversation_id: None,
            provider: "codex".into(),
            model: "m".into(),
            effort: None,
            fast: false,
            state,
            keep_running: false,
            archive_after: None,
            last_stopped_at: None,
            data_expired_at: None,
            created_at: crate::util::now_iso(),
            updated_at: crate::util::now_iso(),
            branch: None,
            repo_project_id: None,
            base_sha: None,
        }
    }

    /// A tree of threads hands off so many since the user last wrote in it, wherever in it they
    /// come from; only the first over is told; the user writing anywhere in it starts again.
    /// Another tree counts its own.
    #[test]
    fn what_a_tree_may_hand_off_since_the_user_wrote() {
        let store = Store::open_in_memory().unwrap();
        for t in ["p", "c", "g", "other"] {
            store.insert_thread(&thread(t, ThreadState::Idle)).unwrap();
        }
        set_parent(&store, "c", "p").unwrap();
        set_parent(&store, "g", "c").unwrap();
        for n in 0..MAX_SINCE_YOU {
            assert_eq!(spend(&store, ["p", "c", "g"][n % 3]), Ok(()), "{n}");
        }
        let over = spend(&store, "g").unwrap_err();
        assert!(
            over.tell
                && over.why
                    == "Hand-off limit reached (20 since your last message). Send a message to continue.",
            "{over:?}"
        );
        assert!(!spend(&store, "p").unwrap_err().tell);
        assert_eq!(spend(&store, "other"), Ok(()), "a tree of its own");
        user_wrote(&store, "g").unwrap();
        assert_eq!(spend(&store, "p"), Ok(()));
        assert!(
            spends(Start::Delegate)
                && spends(Start::Agent)
                && spends(Start::Message { wakes: true })
                && !spends(Start::Message { wakes: false })
        );
    }

    /// One turn starts a few at most, and only the first over is told; each turn counts on its
    /// own, however its requests interleave (one read back late). A chain goes so deep, a
    /// thread keeps so many VMs up (a wake on its way holds a place), and so does its tree.
    #[test]
    fn what_one_thread_may_start() {
        let store = Store::open_in_memory().unwrap();
        store
            .insert_thread(&thread("p", ThreadState::Running))
            .unwrap();
        for _ in 0..MAX_PER_TURN - 1 {
            assert_eq!(this_turn(&store, "p", "turn-1"), Ok(()));
            assert_eq!(
                this_turn(&store, "p", "turn-2"),
                Ok(()),
                "another turn's in between"
            );
        }
        assert_eq!(this_turn(&store, "p", "turn-1"), Ok(()));
        let over = this_turn(&store, "p", "turn-1").unwrap_err();
        assert!(over.tell && over.why.contains("one turn"), "{over:?}");
        assert!(!this_turn(&store, "p", "turn-1").unwrap_err().tell);
        assert_eq!(this_turn(&store, "p", "turn-2"), Ok(()));
        assert!(this_turn(&store, "p", "turn-2").is_err());
        // However many turns come between, a turn's count stays while it's recent; one a day
        // old is let go.
        for n in 0..40 {
            let _ = this_turn(&store, "p", &format!("later-{n}"));
        }
        assert!(
            this_turn(&store, "p", "turn-1").is_err(),
            "turn 1's count was let go"
        );
        let mut kept: Vec<(String, usize, i64)> =
            serde_json::from_str(&store.kv_get(&turn_key("p")).unwrap().unwrap()).unwrap();
        kept.iter_mut()
            .filter(|(b, _, _)| b.starts_with("later"))
            .for_each(|t| t.2 -= TURN_KEPT_MS);
        store
            .kv_set(&turn_key("p"), &serde_json::to_string(&kept).unwrap())
            .unwrap();
        let _ = this_turn(&store, "p", "turn-3");
        let kept: Vec<(String, usize, i64)> =
            serde_json::from_str(&store.kv_get(&turn_key("p")).unwrap().unwrap()).unwrap();
        let mut batches: Vec<&str> = kept.iter().map(|(b, _, _)| b.as_str()).collect();
        batches.sort();
        assert_eq!(batches, ["turn-1", "turn-2", "turn-3"]);
        // Deep: p ← c1 ← c2 ← c3.
        for (child, parent) in [("c1", "p"), ("c2", "c1"), ("c3", "c2")] {
            store
                .insert_thread(&thread(child, ThreadState::Stopped))
                .unwrap();
            set_parent(&store, child, parent).unwrap();
        }
        assert_eq!(depth(&store, "p"), 0);
        assert_eq!(depth(&store, "c3"), MAX_DEPTH);
        assert!(
            room(&store, "c3", Start::Delegate)
                .unwrap_err()
                .why
                .contains("deep")
        );
        assert_eq!(
            room(&store, "c3", Start::Agent),
            Ok(()),
            "an agent in its own VM starts no VM"
        );
        assert_eq!(room(&store, "c2", Start::Delegate), Ok(()));
        // A loop of parents still ends.
        set_parent(&store, "p", "c3").unwrap();
        assert!(depth(&store, "p") > MAX_DEPTH);
        store.kv_delete("parent:p").unwrap();
        // Its own VMs up: p has MAX_LIVE children up; a stopped one doesn't count.
        for i in 0..MAX_LIVE {
            let id = format!("live{i}");
            store
                .insert_thread(&thread(
                    &id,
                    if i % 2 == 0 {
                        ThreadState::Idle
                    } else {
                        ThreadState::Creating
                    },
                ))
                .unwrap();
            set_parent(&store, &id, "p").unwrap();
        }
        let up = room(&store, "p", Start::Delegate).unwrap_err();
        assert!(up.tell && up.why.contains("have their VMs up"), "{up:?}");
        assert!(room(&store, "p", Start::Message { wakes: true }).is_err());
        assert_eq!(room(&store, "p", Start::Message { wakes: false }), Ok(()));
        store
            .set_thread_state("live0", ThreadState::Stopped)
            .unwrap();
        assert_eq!(room(&store, "p", Start::Delegate), Ok(()));
        // A message waking a stopped one holds its place until it's up, or given up on.
        reserve_wake(&store, "live0").unwrap();
        assert!(room(&store, "p", Start::Message { wakes: true }).is_err());
        release_wake(&store, "live0").unwrap();
        assert_eq!(room(&store, "p", Start::Delegate), Ok(()));
        store
            .kv_set(
                &waking_key("live0"),
                &(crate::util::now_ms() - WAKING_MS - 1).to_string(),
            )
            .unwrap();
        assert_eq!(
            room(&store, "p", Start::Delegate),
            Ok(()),
            "a wake long lost"
        );
        // The tree: c1 (no children of its own up) is in p's tree, which has MAX_TREE up.
        for i in 0..(MAX_TREE - MAX_LIVE + 1) {
            let id = format!("deep{i}");
            store
                .insert_thread(&thread(&id, ThreadState::Running))
                .unwrap();
            set_parent(&store, &id, "c2").unwrap();
        }
        let tree = room(&store, "c1", Start::Delegate).unwrap_err();
        assert!(tree.why.contains("from the user's thread"), "{tree:?}");
        // Agents in a thread's own VM.
        for i in 0..MAX_LIVE {
            let side = crate::side::Side {
                id: format!("s{i}"),
                title: "Review".into(),
                provider: "codex".into(),
                model: "m".into(),
                task: "Review it".into(),
                batch: "b".into(),
                conversation: Some(format!("conv{i}")),
                turn: None,
                status: "running".into(),
                error: None,
                calls: 0,
                latest: None,
                answer: None,
                started_ms: 1,
                ended_ms: None,
                reported: false,
            };
            crate::side::save(&store, "c3", &side).unwrap();
        }
        assert!(
            room(&store, "c3", Start::Agent)
                .unwrap_err()
                .why
                .contains("in this VM")
        );
    }

    /// Flotilla's own messages wait while the user has the thread archived, or has stopped its
    /// VM; a VM that stopped by itself, idle, is started for them.
    #[test]
    fn what_may_wake_a_thread() {
        let store = Store::open_in_memory().unwrap();
        store
            .insert_thread(&thread("t", ThreadState::Stopped))
            .unwrap();
        assert!(may_wake(&store, "t"), "stopped by itself");
        store
            .kv_set(&crate::lifecycle::stopped_by_user_key("t"), "1")
            .unwrap();
        assert!(!may_wake(&store, "t"), "the user stopped it");
        store.set_thread_state("t", ThreadState::Idle).unwrap();
        assert!(!may_wake(&store, "t"), "still up as it stops");
        store
            .kv_delete(&crate::lifecycle::stopped_by_user_key("t"))
            .unwrap();
        assert!(may_wake(&store, "t"), "started again");
        store.set_archived("t", true).unwrap();
        assert!(!may_wake(&store, "t"), "archived");
        store.set_archived("t", false).unwrap();
        store.set_thread_state("t", ThreadState::Expired).unwrap();
        assert!(!may_wake(&store, "t"));
        assert!(!may_wake(&store, "gone"));
        hold(&store, "t", "one").unwrap();
        hold(&store, "t", "two").unwrap();
        let texts = |store: &Store| {
            held(store, "t")
                .into_iter()
                .map(|k| k.text)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            (holding(&store), texts(&store)),
            (
                vec!["t".to_string()],
                vec!["one".to_string(), "two".to_string()]
            )
        );
        // In the order they were kept, by a count that only grows (not the clock).
        let ids: Vec<String> = held(&store, "t").into_iter().map(|k| k.id).collect();
        assert!(
            ids.len() == 2 && ids[0] < ids[1] && ids.iter().all(|i| i.len() == 20),
            "{ids:?}"
        );
        // Kept until each reaches it; one kept while another goes is never lost.
        let one = held(&store, "t").remove(0).id;
        hold(&store, "t", "three").unwrap();
        delivered(&store, "t", &one).unwrap();
        assert_eq!(texts(&store), ["two", "three"]);
        // With what it reports, in one write.
        hold_with(&store, "t", "four", vec![("side:t:s1".into(), "{}".into())]).unwrap();
        assert_eq!(texts(&store).last().map(String::as_str), Some("four"));
        assert!(store.kv_get("side:t:s1").unwrap().is_some());
        clear_held(&store, "t").unwrap();
        assert!(holding(&store).is_empty());
    }

    /// A parent waiting on its child: a follow-up's wait begun while the last answer is read
    /// stays, for its own answer; one given up on (its message didn't go) takes no other with
    /// it; a steer that joined a turn is answered with it, once.
    #[test]
    fn each_wait_is_met_or_given_up_by_itself() {
        let store = Store::open_in_memory().unwrap();
        let settle = |id: &str| {
            let mut turn = store
                .turns("c")
                .unwrap()
                .into_iter()
                .find(|t| t.prompt_id == id)
                .unwrap();
            (turn.status, turn.done, turn.settled) = ("finished".into(), true, true);
            store.update_turn(&turn).unwrap();
        };
        // Waits as they were kept before still read.
        store.kv_set(&awaiting_key("c"), "[]").unwrap();
        assert_eq!(awaiting(&store, "c"), Some(vec![]));
        set_awaiting(&store, "c", false).unwrap();
        assert_eq!(awaiting(&store, "c"), None);
        // The first message, answered; the second's wait begun before that answer is read.
        begin_wait(&store, "c").unwrap();
        store
            .insert_sent_prompt("c", "p1", "prompt", "first")
            .unwrap();
        store.insert_turn("c", "p1", "running").unwrap();
        settle("p1");
        let second = begin_wait(&store, "c").unwrap();
        assert!(answer_waits(&store, "c").unwrap(), "the first is answered");
        assert_eq!(
            awaiting(&store, "c"),
            Some(vec!["p1".to_string()]),
            "the second still waits"
        );
        assert!(
            !answer_waits(&store, "c").unwrap(),
            "nothing new for the second yet"
        );
        // Its message didn't go: only its wait goes.
        let third = begin_wait(&store, "c").unwrap();
        end_wait(&store, "c", second).unwrap();
        assert!(awaiting(&store, "c").is_some());
        end_wait(&store, "c", third).unwrap();
        assert_eq!(awaiting(&store, "c"), None);
        // Two follow-ups, the second a steer into the first's turn: one answer for both.
        begin_wait(&store, "c").unwrap();
        store
            .insert_sent_prompt("c", "p2", "prompt", "again")
            .unwrap();
        store.insert_turn("c", "p2", "running").unwrap();
        begin_wait(&store, "c").unwrap();
        store
            .insert_sent_prompt("c", "s2", "steer", "and this")
            .unwrap();
        assert!(
            !answer_waits(&store, "c").unwrap(),
            "its turn is still going"
        );
        settle("p2");
        assert!(answer_waits(&store, "c").unwrap());
        assert_eq!(awaiting(&store, "c"), None, "both met at once");
    }

    /// The user's stop, as boat's readings of the VM come in: through once it's seen stopped;
    /// a reading from before it (a list read as the stop went) leaves it; the VM up again later
    /// than the stop (started from boat's CLI or dashboard) ends it.
    #[test]
    fn the_users_stop_lasts_until_the_vm_is_started_again() {
        let store = Store::open_in_memory().unwrap();
        let sb = |updated: &str, stopped: Option<&str>| crate::boat::Sandbox {
            id: "bx_t".into(),
            state: String::new(),
            name: None,
            labels: None,
            environment: None,
            ttl_seconds: None,
            archive_after: None,
            stopped_at: stopped.map(str::to_string),
            data_expired_at: None,
            created_at: None,
            updated_at: Some(updated.into()),
            machine_type: None,
            stop: None,
        };
        let marked = |store: &Store| crate::lifecycle::stopped_by_user(store, "t");
        let seen = |store: &Store, s: &crate::boat::Sandbox, state| {
            crate::lifecycle::seen_vm(store, "t", s, state)
        };
        store
            .kv_set(
                &crate::lifecycle::stopped_by_user_key("t"),
                "stopping 2026-10-31T09:00:00Z",
            )
            .unwrap();
        seen(
            &store,
            &sb("2026-11-01T10:00:00Z", Some("2026-10-31T09:00:00Z")),
            ThreadState::Idle,
        );
        assert!(marked(&store), "up while it stops");
        seen(
            &store,
            &sb("2026-11-01T10:01:00Z", Some("2026-11-01T10:01:00Z")),
            ThreadState::Stopped,
        );
        assert!(marked(&store));
        seen(
            &store,
            &sb("2026-11-01T10:00:00Z", Some("2026-10-31T09:00:00Z")),
            ThreadState::Idle,
        );
        assert!(marked(&store), "a reading from before the stop");
        seen(
            &store,
            &sb("2026-11-01T11:00:00Z", Some("2026-11-01T10:01:00Z")),
            ThreadState::Running,
        );
        assert!(!marked(&store), "started again elsewhere");
        // Stopped and started again while Flotilla wasn't looking (`flotilla stop --no-wait`
        // with the engine closed, then boat's CLI): the first reading it has shows a later
        // stop, and the VM up.
        store
            .kv_set(
                &crate::lifecycle::stopped_by_user_key("t"),
                "stopping 2026-10-31T09:00:00Z",
            )
            .unwrap();
        seen(
            &store,
            &sb("2026-11-02T08:00:00Z", Some("2026-11-02T07:00:00Z")),
            ThreadState::Idle,
        );
        assert!(!marked(&store), "a stop since, and up again");
        store
            .kv_set(&crate::lifecycle::stopped_by_user_key("t"), "stopping -")
            .unwrap();
        seen(
            &store,
            &sb("2026-11-02T08:00:00Z", None),
            ThreadState::Running,
        );
        assert!(marked(&store), "never stopped yet: still stopping");
        seen(
            &store,
            &sb("2026-11-02T09:00:00Z", Some("2026-11-02T08:30:00Z")),
            ThreadState::Running,
        );
        assert!(!marked(&store));
        // boat might not keep when it last stopped: a later reading still ends it.
        store
            .kv_set(&crate::lifecycle::stopped_by_user_key("t"), "stopping -")
            .unwrap();
        seen(
            &store,
            &sb("2026-11-01T12:00:00Z", None),
            ThreadState::Stopped,
        );
        seen(&store, &sb("2026-11-01T12:00:00Z", None), ThreadState::Idle);
        assert!(marked(&store), "the same reading");
        seen(&store, &sb("2026-11-01T12:30:00Z", None), ThreadState::Idle);
        assert!(!marked(&store));
    }

    /// A thread deleted (or forgotten) takes its hand-offs' keys with it; requests kept from
    /// before they were kept by thread are cleared once.
    #[test]
    fn a_thread_takes_its_keys_with_it() {
        let store = Store::open_in_memory().unwrap();
        for t in ["t1", "t10"] {
            store.insert_thread(&thread(t, ThreadState::Idle)).unwrap();
            key_or_new(&store, t).unwrap();
            set_handled(&store, t, "7c11d703-6d90-4aba-a93a-a86fd03a90f7", "c1").unwrap();
            let _ = this_turn(&store, t, "b");
            reserve_wake(&store, t).unwrap();
            hold(&store, t, "x").unwrap();
            store
                .kv_set(&crate::lifecycle::stopped_by_user_key(t), "1")
                .unwrap();
        }
        store
            .kv_set("orch:req:7c11d703-6d90-4aba-a93a-a86fd03a90f7", "old")
            .unwrap();
        assert_eq!(forget_unowned_requests(&store).unwrap(), 1);
        store.delete_thread("t1").unwrap();
        let mut left: Vec<String> = store
            .kv_with_prefix("")
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .filter(|k| k.contains("t1"))
            .map(|k| {
                if k.starts_with("orch:held:") {
                    "orch:held:t10:…".into()
                } else {
                    k
                }
            })
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "orch:held:t10:…",
                "orch:key:t10",
                "orch:req:t10:7c11d703-6d90-4aba-a93a-a86fd03a90f7",
                "orch:turn:t10",
                "orch:waking:t10",
                "stopped_by_user:t10"
            ]
        );
    }

    /// A child deleted (or forgotten) while its parent waits on it: the parent no longer shows
    /// as waiting, and nothing of the child's hand-off is left. Another thread's stays.
    #[test]
    fn deleting_a_child_ends_its_parents_wait() {
        let store = Store::open_in_memory().unwrap();
        for child in ["c1", "c10"] {
            set_parent(&store, child, "p1").unwrap();
            set_awaiting(&store, child, true).unwrap();
            seen_from_start(&store, child).unwrap();
            store.kv_set(&format!("side:{child}:a1"), "{}").unwrap();
        }
        store.delete_thread("c1").unwrap();
        assert_eq!(parents(&store).into_keys().collect::<Vec<_>>(), ["c10"]);
        assert!(awaiting(&store, "c1").is_none() && awaiting(&store, "c10").is_some());
        assert_eq!(
            store
                .kv_with_prefix("side:")
                .unwrap()
                .into_iter()
                .map(|(k, _)| k)
                .collect::<Vec<_>>(),
            ["side:c10:a1"]
        );
        assert!(store.kv_get(&since_key("c1")).unwrap().is_none());
        store.delete_thread("c10").unwrap();
        assert!(waiting_parents(&store, &parents(&store)).is_empty());
    }

    #[test]
    fn answered_once_what_was_asked_has_had_its_turn() {
        let store = Store::open_in_memory().unwrap();
        let settle = |id: &str| {
            let mut turn = store
                .turns("c1")
                .unwrap()
                .into_iter()
                .find(|t| t.prompt_id == id)
                .unwrap();
            (turn.status, turn.done, turn.settled) = ("finished".into(), true, true);
            store.update_turn(&turn).unwrap();
        };
        // A delegate: waiting from before its first prompt, which shows as sending meanwhile.
        set_awaiting(&store, "c1", true).unwrap();
        let known = awaiting(&store, "c1").unwrap();
        store
            .insert_sent_prompt("c1", "local-1", "prompt", "do it")
            .unwrap();
        assert!(
            !answered(&store, "c1", &known).unwrap(),
            "a placeholder isn't delivered"
        );
        store
            .insert_sent_prompt("c1", "p1", "prompt", "do it")
            .unwrap();
        assert!(
            !answered(&store, "c1", &known).unwrap(),
            "no turn for it yet"
        );
        store.insert_turn("c1", "p1", "running").unwrap();
        assert!(!answered(&store, "c1", &known).unwrap());
        settle("p1");
        assert!(answered(&store, "c1", &known).unwrap());
        assert!(answer_waits(&store, "c1").unwrap());
        // A follow-up: what was there before it doesn't count, however recent.
        set_awaiting(&store, "c1", true).unwrap();
        let known = awaiting(&store, "c1").unwrap();
        assert!(!answered(&store, "c1", &known).unwrap());
        store
            .insert_sent_prompt("c1", "p2", "prompt", "and then")
            .unwrap();
        store.insert_turn("c1", "p2", "queued").unwrap();
        assert!(!answered(&store, "c1", &known).unwrap());
        settle("p2");
        assert!(answered(&store, "c1", &known).unwrap());
        set_awaiting(&store, "c1", false).unwrap();
        assert_eq!(awaiting(&store, "c1"), None);
    }
}
