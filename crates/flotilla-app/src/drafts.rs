//! What was being written and not sent: in each thread's composer, and on New thread. Kept
//! for This Mac and the paired server apart, in memory, so switching between them neither
//! loses it nor puts it in front of the other's threads (a server made from This Mac's
//! database has the same thread ids).

use flotilla_proto::Attached;
use std::collections::HashMap;

/// A composer's text and files.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Draft {
    pub text: String,
    pub files: Vec<Attached>,
}

impl Draft {
    /// Something written or attached; None if there's nothing to keep.
    pub fn of(text: String, files: Vec<Attached>) -> Option<Draft> {
        (!text.trim().is_empty() || !files.is_empty()).then_some(Draft { text, files })
    }
}

/// One server's drafts.
#[derive(Debug, Default)]
struct Kept {
    threads: HashMap<String, Draft>,
    new_thread: Option<Draft>,
}

#[derive(Debug, Default)]
pub struct Drafts {
    /// The server shown now (`Device::key`), and its drafts.
    here: String,
    kept: Kept,
    /// Other servers', until they're shown again.
    away: HashMap<String, Kept>,
}

impl Drafts {
    /// Keep what a thread's composer had as it closed (None: nothing, so forget any).
    pub fn keep(&mut self, thread: String, draft: Option<Draft>) {
        match draft {
            Some(d) => {
                self.kept.threads.insert(thread, d);
            }
            None => {
                self.kept.threads.remove(&thread);
            }
        }
    }

    /// What `thread`'s composer had, to put back as it opens.
    pub fn take(&mut self, thread: &str) -> Option<Draft> {
        self.kept.threads.remove(thread)
    }

    pub fn keep_new_thread(&mut self, draft: Option<Draft>) {
        self.kept.new_thread = draft;
    }

    pub fn take_new_thread(&mut self) -> Option<Draft> {
        self.kept.new_thread.take()
    }

    /// `server` is shown now: the last one's drafts are put away, and its own come back.
    pub fn switch(&mut self, server: &str) {
        if server == self.here {
            return;
        }
        let back = self.away.remove(server).unwrap_or_default();
        let gone = std::mem::replace(&mut self.kept, back);
        if !gone.threads.is_empty() || gone.new_thread.is_some() {
            self.away
                .insert(std::mem::replace(&mut self.here, server.to_string()), gone);
        } else {
            self.here = server.to_string();
        }
    }

    /// Only `servers`' drafts stay (This Mac's and the paired server's): a server forgotten
    /// takes its own with it, so pairing it again starts clean.
    pub fn keep_only(&mut self, servers: &[String]) {
        self.away.retain(|k, _| servers.contains(k));
        if !servers.contains(&self.here) {
            self.kept = Kept::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> Option<Draft> {
        Draft::of(text.into(), vec![])
    }

    #[test]
    fn drafts_stay_with_their_server_across_switches() {
        let mut drafts = Drafts::default();
        drafts.switch("this-mac");
        drafts.keep("t1".into(), d("half a thought"));
        drafts.keep_new_thread(d("a new task"));
        drafts.keep("t2".into(), d("  "));
        assert_eq!(drafts.take("t2"), None, "whitespace isn't a draft");

        // To a server with a thread of the same id (made from this Mac's database): nothing
        // of This Mac's shows there.
        drafts.switch("srv_box");
        assert_eq!((drafts.take("t1"), drafts.take_new_thread()), (None, None));
        drafts.keep("t1".into(), d("for the box"));

        // Back: This Mac's are all there, and the box's wait for it.
        drafts.switch("this-mac");
        assert_eq!(drafts.take("t1"), d("half a thought"));
        assert_eq!(drafts.take_new_thread(), d("a new task"));
        drafts.switch("srv_box");
        assert_eq!(drafts.take("t1"), d("for the box"));
        // Taken is gone; switching to where it is already changes nothing.
        drafts.switch("srv_box");
        assert_eq!(drafts.take("t1"), None);
        // Cleared in the composer: forgotten.
        drafts.keep("t3".into(), d("x"));
        drafts.keep("t3".into(), None);
        assert_eq!(drafts.take("t3"), None);
    }

    /// Replacing a server, or forgetting it, takes its drafts with it: paired again, it starts
    /// clean. This Mac's stay.
    #[test]
    fn a_server_forgotten_takes_its_drafts() {
        let mut drafts = Drafts::default();
        drafts.switch("this-mac");
        drafts.keep("t1".into(), d("on this Mac"));
        drafts.switch("srv_a");
        drafts.keep("t1".into(), d("on A"));
        // A replaced by B while A is shown.
        drafts.keep_only(&["this-mac".into(), "srv_b".into()]);
        drafts.switch("srv_b");
        drafts.keep("t1".into(), d("on B"));
        // A paired again, in B's place.
        drafts.keep_only(&["this-mac".into(), "srv_a".into()]);
        drafts.switch("srv_a");
        assert_eq!(drafts.take("t1"), None, "A's old draft is gone");
        drafts.switch("srv_b");
        assert_eq!(drafts.take("t1"), None, "so is B's");
        // Forgotten while This Mac is shown: This Mac's stay.
        drafts.switch("this-mac");
        drafts.keep_only(&["this-mac".into()]);
        assert_eq!(drafts.take("t1"), d("on this Mac"));
    }
}
