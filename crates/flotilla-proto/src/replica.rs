//! A client's copy of the views it subscribes to, kept up to date from the server's snapshots
//! and deltas. It checks every revision: a delta that doesn't start from what it holds, or
//! that it can't apply, is a doubt, and the view is asked for whole (`Up::Resync`). Data that
//! doesn't read as the view (a newer server's) is kept, and later deltas applied to it, but
//! it isn't asked for again: the server would only send the same. It hands the client a view
//! only when it changed (or first arrives after a subscribe).

use crate::wire::{Down, Held, Op, Welcome};
use crate::{View, ViewKey};
use serde_json::Value;
use std::collections::BTreeMap;

/// What a message from the server means for the client.
#[derive(Debug, PartialEq)]
pub enum Taken {
    /// A view to show.
    View(Box<View>),
    /// A view the client lost track of: ask for it whole (`Up::Resync`).
    Resync(ViewKey, String),
    /// A view whose data doesn't read (a newer server's): nothing to show, nothing to ask.
    Unreadable(ViewKey, String),
    Welcome(Welcome),
    /// Caught up after the hello.
    Synced,
    /// The server refused what was sent, and closes.
    Error(String),
    /// An answer, note, progress or live text: hand it on.
    Pass(Down),
    Nothing,
}

#[derive(Debug, Default)]
pub struct Replica {
    /// The server run the revisions are from.
    epoch: Option<String>,
    views: BTreeMap<ViewKey, Copy>,
}

#[derive(Debug, Default)]
struct Copy {
    /// The revision held and the view's data at it, or None while a snapshot is awaited.
    held: Option<(u64, Value)>,
    /// The view as last handed on; None until the first since subscribing.
    shown: Option<View>,
    /// The data held didn't read as the view (a newer server's snapshot).
    unreadable: bool,
}

/// What a message from the server came to.
#[derive(Debug, PartialEq)]
pub enum Applied {
    /// The view changed (or arrived): show it.
    Changed(Box<View>),
    /// Nothing new to show.
    Same,
    /// It couldn't be applied: ask for the view whole.
    Doubt(String),
    /// A snapshot (or a delta on one) that doesn't read as the view: kept, and not asked for
    /// again.
    Unreadable(String),
}

impl Replica {
    pub fn new() -> Replica {
        Replica::default()
    }

    pub fn epoch(&self) -> Option<&str> {
        self.epoch.as_deref()
    }

    /// A fresh subscribe: whatever was held of it goes, and its first snapshot is handed on.
    pub fn subscribe(&mut self, view: ViewKey) {
        self.views.insert(view, Copy::default());
    }

    pub fn unsubscribe(&mut self, view: &ViewKey) {
        self.views.remove(view);
    }

    pub fn subscribed(&self) -> impl Iterator<Item = &ViewKey> {
        self.views.keys()
    }

    pub fn is_subscribed(&self, view: &ViewKey) -> bool {
        self.views.contains_key(view)
    }

    /// What to say in a hello: every view subscribed to, with the revision held.
    pub fn held(&self) -> Vec<Held> {
        self.views
            .iter()
            .map(|(view, c)| Held {
                view: view.clone(),
                rev: c.held.as_ref().map(|(rev, _)| *rev),
            })
            .collect()
    }

    /// The server's run: from another one, no revision held means anything.
    pub fn welcome(&mut self, epoch: &str) {
        if self.epoch.as_deref() != Some(epoch) {
            for c in self.views.values_mut() {
                c.held = None;
            }
            self.epoch = Some(epoch.to_string());
        }
    }

    /// Asked for whole again: wait for its snapshot.
    pub fn resyncing(&mut self, view: &ViewKey) {
        if let Some(c) = self.views.get_mut(view) {
            c.held = None;
        }
    }

    pub fn snap(&mut self, view: &ViewKey, rev: u64, data: Value) -> Applied {
        let Some(c) = self.views.get_mut(view) else {
            return Applied::Same;
        };
        c.held = Some((rev, data.clone()));
        // The whole view: if it doesn't read, asking again gets the same.
        match Self::show(view, data, c) {
            Err(why) => Applied::Unreadable(why),
            Ok(a) => a,
        }
    }

    pub fn delta(&mut self, view: &ViewKey, base: u64, rev: u64, ops: &[Op]) -> Applied {
        let Some(c) = self.views.get_mut(view) else {
            return Applied::Same;
        };
        // Waiting for a snapshot: nothing to do.
        let Some((held, data)) = c.held.as_mut() else {
            return Applied::Same;
        };
        // This one is already applied.
        if rev <= *held {
            return Applied::Same;
        }
        if *held != base {
            let why = format!("a delta from {base} to {rev}, holding {held}");
            c.held = None;
            return Applied::Doubt(why);
        }
        if let Err(e) = crate::delta::apply(view, data, ops) {
            c.held = None;
            return Applied::Doubt(e);
        }
        *held = rev;
        let data = data.clone();
        // On data that read, a delta that leaves it not reading may be a mistake: asked for
        // whole. On data that didn't read already, it's the same as the snapshot.
        let was = c.unreadable;
        match Self::show(view, data, c) {
            Err(why) if was => Applied::Unreadable(why),
            Err(why) => {
                c.held = None;
                Applied::Doubt(why)
            }
            Ok(a) => a,
        }
    }

    /// `data` (what `c` now holds) as the view; `Err` if it doesn't read as one.
    fn show(view: &ViewKey, data: Value, c: &mut Copy) -> Result<Applied, String> {
        let v = View::from_data(view, data);
        c.unreadable = v.is_err();
        let v = v?;
        if c.shown.as_ref() == Some(&v) {
            return Ok(Applied::Same);
        }
        c.shown = Some(v.clone());
        Ok(Applied::Changed(Box::new(v)))
    }

    /// Take a message from the server: keep the views up to date, and say what to do.
    pub fn take(&mut self, down: Down) -> Taken {
        let (key, applied) = match down {
            Down::Snap { view, rev, data } => {
                let applied = self.snap(&view, rev, data);
                (view, applied)
            }
            Down::Delta {
                view,
                base,
                rev,
                ops,
            } => {
                let applied = self.delta(&view, base, rev, &ops);
                (view, applied)
            }
            Down::Welcome(w) => {
                self.welcome(&w.epoch);
                return Taken::Welcome(w);
            }
            Down::Synced => return Taken::Synced,
            Down::Error { msg } => return Taken::Error(msg),
            down @ (Down::Res { .. }
            | Down::Live { .. }
            | Down::Note { .. }
            | Down::Progress { .. }) => return Taken::Pass(down),
        };
        match applied {
            Applied::Changed(view) => Taken::View(view),
            Applied::Same => Taken::Nothing,
            Applied::Doubt(why) => Taken::Resync(key, why),
            Applied::Unreadable(why) => Taken::Unreadable(key, why),
        }
    }

    /// The view's data as held (tests compare it with the server's).
    pub fn data(&self, view: &ViewKey) -> Option<&Value> {
        self.views.get(view)?.held.as_ref().map(|(_, data)| data)
    }

    /// The view as last handed on.
    pub fn view(&self, view: &ViewKey) -> Option<&View> {
        self.views.get(view)?.shown.as_ref()
    }

    pub fn rev(&self, view: &ViewKey) -> Option<u64> {
        self.views.get(view)?.held.as_ref().map(|(rev, _)| *rev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Catalog, View};
    use serde_json::json;

    #[test]
    fn checks_revisions_and_hands_on_only_changes() {
        let mut r = Replica::new();
        r.welcome("e1");
        let cat = ViewKey::Catalog;
        r.subscribe(cat.clone());
        assert!(r.held()[0].rev.is_none());
        // The first snapshot is handed on even if it's empty.
        assert_eq!(
            r.snap(&cat, 3, json!(null)),
            Applied::Changed(Box::new(View::Catalog(None)))
        );
        // The same again isn't.
        assert_eq!(r.snap(&cat, 4, json!(null)), Applied::Same);
        assert_eq!(r.rev(&cat), Some(4));
        // A stale or repeated delta is skipped; one from a revision not held is a doubt.
        assert_eq!(r.delta(&cat, 2, 4, &[]), Applied::Same);
        assert!(matches!(r.delta(&cat, 5, 6, &[]), Applied::Doubt(_)));
        assert_eq!(r.rev(&cat), None);
        // While a snapshot is awaited, deltas wait too.
        assert_eq!(r.delta(&cat, 4, 7, &[]), Applied::Same);
        let catalog = Catalog { providers: vec![] };
        assert_eq!(
            r.snap(&cat, 8, serde_json::to_value(&catalog).unwrap()),
            Applied::Changed(Box::new(View::Catalog(Some(catalog))))
        );
        // Another server run: nothing held counts.
        r.welcome("e2");
        assert_eq!(r.held()[0].rev, None);
        assert_eq!(r.data(&cat), None);
        // Not subscribed: ignored.
        assert_eq!(r.snap(&ViewKey::Settings, 1, json!({})), Applied::Same);
    }

    #[test]
    fn a_delta_that_doesnt_fit_or_doesnt_read_is_a_doubt() {
        let mut r = Replica::new();
        let t = ViewKey::Mentions("t1".into());
        r.subscribe(t.clone());
        assert!(matches!(
            r.snap(&t, 1, json!({"thread": "t1", "mentions": null})),
            Applied::Changed(_)
        ));
        // A field that doesn't read as the view.
        assert!(matches!(
            r.delta(
                &t,
                1,
                2,
                &[Op::Set {
                    field: "mentions".into(),
                    v: json!(5)
                }]
            ),
            Applied::Doubt(_)
        ));
        assert_eq!(r.rev(&t), None);
    }

    /// A snapshot that doesn't read (a newer server's) isn't asked for again, which would only
    /// get the same, over and over: it's kept with its revision, deltas go on applying to it,
    /// and once it reads it's shown.
    #[test]
    fn a_snapshot_that_doesnt_read_is_kept_not_asked_for_again() {
        let mut r = Replica::new();
        let t = ViewKey::Mentions("t1".into());
        r.subscribe(t.clone());
        let odd = json!({"thread": "t1", "mentions": 5});
        assert!(matches!(
            r.take(Down::Snap {
                view: t.clone(),
                rev: 1,
                data: odd.clone()
            }),
            Taken::Unreadable(..)
        ));
        assert_eq!(r.rev(&t), Some(1));
        // A delta on it that still doesn't read: the same.
        assert!(matches!(
            r.delta(
                &t,
                1,
                2,
                &[Op::Set {
                    field: "extra".into(),
                    v: json!(1)
                }]
            ),
            Applied::Unreadable(_)
        ));
        assert_eq!(r.rev(&t), Some(2));
        // One that makes it read: shown.
        assert!(matches!(
            r.delta(
                &t,
                2,
                3,
                &[Op::Set {
                    field: "mentions".into(),
                    v: json!(null)
                }]
            ),
            Applied::Changed(_)
        ));
        assert_eq!(r.rev(&t), Some(3));
    }

    /// A newer server's values this version doesn't know read as the iPhone reads them, rather
    /// than failing the whole view.
    #[test]
    fn a_value_this_version_doesnt_know_reads_as_the_iphone_reads_it() {
        let mut r = Replica::new();
        let side = ViewKey::Sidebar;
        r.subscribe(side.clone());
        let View::Sidebar(sidebar) = crate::tests::sample_views().remove(0) else {
            unreachable!()
        };
        let mut data = serde_json::to_value(&sidebar).unwrap();
        data["engine"] = json!("paused");
        data["threads"][0]["thread"]["state"] = json!("hibernating");
        data["threads"][0]["activity"] = json!("dreaming");
        let Applied::Changed(v) = r.snap(&side, 1, data) else {
            panic!("didn't read")
        };
        let View::Sidebar(got) = *v else {
            unreachable!()
        };
        assert_eq!(got.engine, crate::EngineState::Offline);
        assert_eq!(got.threads[0].thread.state, crate::ThreadState::Error);
        assert_eq!(got.threads[0].activity, crate::Activity::Idle);
    }

    #[test]
    fn a_doubt_lets_go_of_the_data_and_keeps_what_was_shown() {
        let mut r = Replica::new();
        let t = ViewKey::Mentions("t1".into());
        r.subscribe(t.clone());
        let first = json!({"thread": "t1", "mentions": null});
        assert!(matches!(r.snap(&t, 1, first.clone()), Applied::Changed(_)));
        let shown = r.view(&t).cloned();
        assert!(shown.is_some());
        // A delta that changes a field and then fails: none of it is held.
        let ops = [
            Op::Set {
                field: "mentions".into(),
                v: json!([]),
            },
            Op::Del {
                field: "mentions".into(),
                keys: vec![json!("x")],
            },
        ];
        assert!(matches!(r.delta(&t, 1, 2, &ops), Applied::Doubt(_)));
        assert_eq!((r.rev(&t), r.data(&t)), (None, None));
        assert_eq!(r.view(&t), shown.as_ref());
        // Its snapshot, the same as was shown, isn't handed on again.
        assert_eq!(r.snap(&t, 3, first.clone()), Applied::Same);
        assert_eq!((r.rev(&t), r.data(&t)), (Some(3), Some(&first)));
        // A gap, and data that doesn't read as the view: the same.
        assert!(matches!(r.delta(&t, 4, 5, &[]), Applied::Doubt(_)));
        assert_eq!((r.rev(&t), r.data(&t)), (None, None));
        // A snapshot that doesn't read as the view: kept with its revision, and not asked for
        // again (asking gets the same); what was shown stays.
        let odd = json!({"thread": "t2", "mentions": null});
        assert!(matches!(r.snap(&t, 6, odd.clone()), Applied::Unreadable(_)));
        assert_eq!((r.rev(&t), r.data(&t)), (Some(6), Some(&odd)));
        assert_eq!(r.view(&t), shown.as_ref());
    }
}
