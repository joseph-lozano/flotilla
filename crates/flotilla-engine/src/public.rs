//! Public links' clock (`flotilla_core::public`): the worker looks every few seconds, whether
//! or not an app is open, and makes each link private when its time is up. What it needs is
//! kept in the database, so a restart loses nothing: a link due while the server was off ends
//! at the first look. It also ends links that ended while their VM was stopped once the VM is
//! up again from outside Flotilla (a wake through Flotilla ends them itself), and forgets those
//! whose VM is gone.

use super::{Changes, Event, Events};
use flotilla_core::boat::BoatClient;
use flotilla_core::public::Sweeper;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

/// How long between looks, at most: a link ends within this of its time.
const LOOK_EVERY: Duration = Duration::from_secs(5);
/// How soon to look again while ends are under way, or left for a thread busy with a preview.
const BUSY_AGAIN: Duration = Duration::from_secs(1);

pub(crate) struct Worker {
    /// Starts each look's ends on workers of their own and comes straight back, so the clock
    /// never waits for boat; they say what changed as they finish.
    sweeper: Sweeper,
}

impl Worker {
    pub(crate) fn new(db: PathBuf, boat: BoatClient, events: Events) -> Worker {
        let sweeper = Sweeper::new(boat, db, move |changed| {
            let mut c = Changes::default();
            for t in &changed {
                c.add_thread(t);
            }
            let _ = events.send_blocking(Event::Changed(c));
        });
        Worker { sweeper }
    }

    /// Until the engine stops (its sender is dropped).
    pub(crate) fn run(self, stop: mpsc::Receiver<()>) {
        loop {
            let wait = self.look(flotilla_core::util::now_ms());
            match stop.recv_timeout(wait) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// Start ending what's due at `now`, and how long until the next look.
    pub(crate) fn look(&self, now: i64) -> Duration {
        let swept = self.sweeper.look(now);
        let next = swept.next_ms.map_or(LOOK_EVERY, |at| {
            Duration::from_millis((at - now).max(0) as u64)
        });
        let next = if swept.busy || swept.failed {
            next.min(BUSY_AGAIN)
        } else {
            next
        };
        next.clamp(Duration::from_millis(200), LOOK_EVERY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flotilla_core::public::{self, testing};
    use flotilla_core::store::{Store, Thread, ThreadState};
    use flotilla_proto::PublicFor;

    const HOUR: i64 = 3_600_000;

    fn thread(id: &str) -> Thread {
        Thread {
            id: id.into(),
            environment_id: Some("e1".into()),
            environment_name: Some("base".into()),
            title: format!("Thread {id}"),
            sandbox_id: Some(format!("bx_{id}")),
            conversation_id: Some(format!("conv_{id}")),
            provider: "claude-code".into(),
            model: "haiku".into(),
            effort: None,
            fast: false,
            state: ThreadState::Idle,
            keep_running: false,
            archive_after: None,
            last_stopped_at: None,
            data_expired_at: None,
            created_at: flotilla_core::util::now_iso(),
            updated_at: flotilla_core::util::now_iso(),
            branch: None,
            repo_project_id: None,
            base_sha: None,
        }
    }

    /// The worker ends a link at its time, says the thread changed, and waits no longer than
    /// until the next one is due; started again (a restart), it ends one that came due while it
    /// was off. One until stopped is never ended.
    #[test]
    fn the_worker_ends_links_on_time_and_after_a_restart() {
        let dir =
            std::env::temp_dir().join(format!("flotilla-public-worker-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("flotilla.db");
        let store = Store::open(&db).unwrap();
        let t = thread("t1");
        store.insert_thread(&t).unwrap();
        let (boat, held) = testing::serve();
        public::make_public(&boat, &store, &t, 3000, PublicFor::Hour, 0).unwrap();
        public::make_public(&boat, &store, &t, 3001, PublicFor::Day, 0).unwrap();
        public::make_public(&boat, &store, &t, 4100, PublicFor::UntilStopped, 0).unwrap();
        let (tx, rx) = mpsc::channel();
        let worker = Worker::new(
            db.clone(),
            boat.clone(),
            Events::new(move |e| tx.send(e).is_ok()),
        );
        let settled = |w: &Worker| assert!(w.sweeper.settle(Duration::from_secs(10)));
        // A second before the hour: nothing yet, and the next look comes at it.
        assert_eq!(worker.look(HOUR - 1_000), Duration::from_secs(1));
        assert!(rx.try_recv().is_err());
        assert_eq!(
            worker.look(HOUR - 100),
            Duration::from_millis(200),
            "never sooner than 200 ms"
        );
        // Far off: at most five seconds.
        assert_eq!(worker.look(0), LOOK_EVERY);
        // Due: its end starts, and the next look comes soon while it's under way.
        assert_eq!(worker.look(HOUR), BUSY_AGAIN);
        settled(&worker);
        assert!(!held.lock().unwrap().public("bx_t1", 3000));
        assert!(
            held.lock().unwrap().public("bx_t1", 3001)
                && held.lock().unwrap().public("bx_t1", 4100)
        );
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Event::Changed(c)) => assert!(c.has_thread("t1"), "{c:?}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(worker.look(HOUR), LOOK_EVERY, "nothing under way");
        // Off for two days, then started again.
        drop(worker);
        let (tx, rx) = mpsc::channel();
        let again = Worker::new(db.clone(), boat, Events::new(move |e| tx.send(e).is_ok()));
        again.look(2 * 24 * HOUR);
        settled(&again);
        assert!(!held.lock().unwrap().public("bx_t1", 3001));
        assert!(
            matches!(rx.recv_timeout(Duration::from_secs(5)), Ok(Event::Changed(c)) if c.has_thread("t1"))
        );
        // Until stopped: a year on, it still is.
        again.look(365 * 24 * HOUR);
        settled(&again);
        assert!(held.lock().unwrap().public("bx_t1", 4100));
        assert!(rx.try_recv().is_err());
        assert_eq!(
            public::shown(&store, &t)
                .iter()
                .map(|l| l.port)
                .collect::<Vec<_>>(),
            [4100]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
