//! What `follow` has printed of a thread, so each look prints only what's new: messages as
//! they settle, in order, and streaming text as it grows, whose final message then prints
//! only the rest. Both `follow`s use it: the one that polls boat itself and the one that
//! reads a server's views.

use crate::message_text;
use flotilla_proto::{Message, RawEvent};
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};

#[derive(Default)]
pub(crate) struct Shown {
    /// Messages printed, by event and tool call and which of those they are, not by seq: a
    /// server's view holds only the newest messages, so where it starts moves on.
    printed: HashSet<(String, String, usize)>,
    /// Partial text on screen, by the partial's timestamp (its final message has it too).
    streamed: HashMap<i64, String>,
    /// Prompts and steers printed while this machine's placeholders (`local-…`, sending), by
    /// their key, with their text: once boat takes one it's listed under boat's id, the same
    /// message under a new key, not a new one.
    placeholders: Vec<((String, String, usize), String)>,
}

/// A prompt or steer still this machine's placeholder, its text: boat hasn't given it an id yet.
fn placeholder(m: &Message) -> Option<&str> {
    let said = matches!(m.kind.as_str(), "prompt" | "steer");
    m.task_id
        .as_deref()
        .filter(|t| said && t.starts_with(flotilla_model::lifecycle::LOCAL_PREFIX))
        .map(|_| m.text.as_deref().unwrap_or(""))
}

/// Each message's key for `printed`.
fn ident(msgs: &[Message]) -> Vec<(String, String, usize)> {
    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    msgs.iter()
        .map(|m| {
            let k = (
                format!(
                    "{}/{}",
                    m.event_key.as_deref().unwrap_or_default(),
                    m.tool_id.as_deref().unwrap_or_default()
                ),
                m.kind.clone(),
            );
            let n = counts.entry(k.clone()).or_default();
            *n += 1;
            (k.0, k.1, *n)
        })
        .collect()
}

impl Shown {
    /// What the thread already shows counts as printed.
    pub fn seed(&mut self, msgs: &[Message]) {
        for (m, id) in msgs.iter().zip(ident(msgs)) {
            if let Some(text) = placeholder(m) {
                self.placeholders.push((id.clone(), text.to_string()));
            }
            self.printed.insert(id);
        }
    }

    /// Print the messages not printed yet, in order, up to a tool call still running. True if
    /// it stopped at one: streaming text waits until it's done.
    pub fn settle(&mut self, out: &mut impl Write, msgs: &[Message]) -> io::Result<bool> {
        let idents = ident(msgs);
        let listed: HashSet<&(String, String, usize)> = idents.iter().collect();
        for (m, id) in msgs.iter().zip(idents.iter().cloned()) {
            if self.printed.contains(&id) {
                continue;
            }
            if m.kind == "tool" && m.pending {
                return Ok(true);
            }
            // A placeholder printed, gone from the list, and a prompt or steer saying the same
            // in its place: it's that one, under boat's id.
            if matches!(m.kind.as_str(), "prompt" | "steer")
                && placeholder(m).is_none()
                && let Some(i) = self.placeholders.iter().position(|(key, text)| {
                    !listed.contains(key) && m.text.as_deref() == Some(text.as_str())
                })
            {
                self.placeholders.remove(i);
                self.printed.insert(id);
                continue;
            }
            if let Some(text) = placeholder(m) {
                self.placeholders.push((id.clone(), text.to_string()));
            }
            // Streamed already: only the rest.
            if m.kind == "text"
                && let Some(shown) = m.timestamp.and_then(|ts| self.streamed.remove(&ts))
            {
                let text = m.text.as_deref().unwrap_or("");
                match text.strip_prefix(shown.as_str()) {
                    Some(rest) => writeln!(out, "{rest}")?,
                    None => writeln!(out, "\n{text}")?,
                }
            } else {
                out.write_all(message_text(m, false, "").as_bytes())?;
            }
            self.printed.insert(id);
        }
        Ok(false)
    }

    /// Streaming text, oldest first: what each partial adds, or the whole of one that changed.
    pub fn stream(&mut self, out: &mut impl Write, partials: &[RawEvent]) -> io::Result<()> {
        let mut partials: Vec<_> = partials.iter().collect();
        partials.sort_by_key(|p| p.timestamp);
        for p in partials {
            let content = p
                .data
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let shown = self.streamed.entry(p.timestamp).or_default();
            match content.strip_prefix(shown.as_str()) {
                Some(rest) => write!(out, "{rest}")?,
                None => write!(out, "\n{content}")?,
            }
            *shown = content.to_string();
        }
        out.flush()
    }

    /// The turn is over: end the line streaming text left open.
    pub fn finish(&self, out: &mut impl Write) -> io::Result<()> {
        if !self.streamed.is_empty() {
            writeln!(out)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(key: &str, ts: i64, t: &str) -> Message {
        Message {
            kind: "text".into(),
            text: Some(t.into()),
            event_key: Some(key.into()),
            timestamp: Some(ts),
            ..Default::default()
        }
    }

    fn tool(key: &str, id: &str, pending: bool) -> Message {
        Message {
            kind: "tool".into(),
            tool_name: Some("Bash".into()),
            tool_id: Some(id.into()),
            tool_summary: Some(id.into()),
            pending,
            event_key: Some(key.into()),
            ..Default::default()
        }
    }

    fn partial(ts: i64, content: &str) -> RawEvent {
        RawEvent {
            id: format!("p{ts}"),
            kind: "partial".into(),
            timestamp: ts,
            task_id: None,
            conversation_id: None,
            data: serde_json::json!({ "content": content }),
            raw: serde_json::Value::Null,
        }
    }

    /// What one look printed.
    fn look(shown: &mut Shown, msgs: &[Message], partials: &[RawEvent]) -> (String, bool) {
        let mut out = Vec::new();
        let blocked = shown.settle(&mut out, msgs).unwrap();
        if !blocked {
            shown.stream(&mut out, partials).unwrap();
        }
        (String::from_utf8(out).unwrap(), blocked)
    }

    fn steer(task: &str, key: &str, status: &str) -> Message {
        Message {
            kind: "steer".into(),
            text: Some("Also run date -u.".into()),
            task_id: Some(task.into()),
            status: Some(status.into()),
            event_key: Some(key.into()),
            ..Default::default()
        }
    }

    /// A steer sent from another shell: its placeholder, then the same steer under the id boat
    /// gave it (still sending), then boat's own: printed once. One saying the same sent after
    /// it is printed too.
    #[test]
    fn a_steer_is_printed_once_as_it_takes_boats_id() {
        let mut shown = Shown::default();
        let p = text("e1", 1, "Working on it.");
        let sending = steer("local-9", "steer-local-9", "sending");
        assert_eq!(
            look(&mut shown, &[p.clone(), sending.clone()], &[]).0,
            "Working on it.\n>> steer: Also run date -u.  [sending]\n"
        );
        let taken = steer("61f3", "steer-61f3", "sending");
        assert_eq!(
            look(&mut shown, &[p.clone(), taken], &[]).0,
            "",
            "the same steer, under boat's id"
        );
        let boats = steer("61f3", "steer-61f3", "native");
        assert_eq!(look(&mut shown, &[p.clone(), boats.clone()], &[]).0, "");
        let again = steer("local-10", "steer-local-10", "sending");
        assert_eq!(
            look(&mut shown, &[p.clone(), boats.clone(), again.clone()], &[]).0,
            ">> steer: Also run date -u.  [sending]\n"
        );
        // Its placeholder still listed: another saying the same is its own.
        let mut shown = Shown::default();
        shown.seed(&[p.clone(), sending.clone()]);
        assert_eq!(
            look(&mut shown, &[p.clone(), sending, boats.clone()], &[]).0,
            ">> steer: Also run date -u.  [native]\n"
        );
        // Seeded as a placeholder (sent just before the follow began): not printed when it
        // takes boat's id.
        let mut shown = Shown::default();
        shown.seed(&[p.clone(), again]);
        assert_eq!(look(&mut shown, &[p, boats], &[]).0, "");
    }

    #[test]
    fn streamed_text_then_only_the_rest_of_its_message() {
        let mut shown = Shown::default();
        assert_eq!(
            look(&mut shown, &[], &[partial(7, "Hello")]),
            ("Hello".into(), false)
        );
        assert_eq!(
            look(&mut shown, &[], &[partial(7, "Hello, wor")]),
            (", wor".into(), false)
        );
        // Its final message: the rest, and the line ends.
        assert_eq!(
            look(&mut shown, &[text("e1", 7, "Hello, world.")], &[]),
            ("ld.\n".into(), false)
        );
        // Printed once.
        assert_eq!(
            look(&mut shown, &[text("e1", 7, "Hello, world.")], &[]),
            ("".into(), false)
        );
        let mut out = Vec::new();
        shown.finish(&mut out).unwrap();
        assert!(out.is_empty(), "nothing left open");
    }

    #[test]
    fn a_partial_that_changed_starts_a_new_line() {
        let mut shown = Shown::default();
        look(&mut shown, &[], &[partial(7, "Hello")]);
        assert_eq!(
            look(&mut shown, &[], &[partial(7, "Goodbye")]),
            ("\nGoodbye".into(), false)
        );
        // A final that isn't what was streamed is printed whole.
        assert_eq!(
            look(&mut shown, &[text("e1", 7, "Farewell")], &[]),
            ("\nFarewell\n".into(), false)
        );
        // Partials go oldest first; one still on screen at the end closes its line.
        assert_eq!(
            look(&mut shown, &[], &[partial(9, "b"), partial(8, "a")]),
            ("ab".into(), false)
        );
        let mut out = Vec::new();
        shown.finish(&mut out).unwrap();
        assert_eq!(out, b"\n");
    }

    #[test]
    fn a_running_tool_holds_back_what_follows() {
        let mut shown = Shown::default();
        let running = [
            text("e1", 1, "Looking."),
            tool("e2", "toolu_1", true),
            text("e3", 3, "Done."),
        ];
        let (said, blocked) = look(&mut shown, &running, &[partial(4, "streaming")]);
        assert_eq!((said.as_str(), blocked), ("Looking.\n", true));
        let done = [
            text("e1", 1, "Looking."),
            tool("e2", "toolu_1", false),
            text("e3", 3, "Done."),
        ];
        assert_eq!(
            look(&mut shown, &done, &[]),
            ("  · Bash toolu_1\nDone.\n".into(), false)
        );
    }

    #[test]
    fn what_was_there_is_not_printed_again() {
        let mut shown = Shown::default();
        let before = [text("e1", 1, "Earlier."), tool("e2", "toolu_1", false)];
        shown.seed(&before);
        let after = [
            text("e1", 1, "Earlier."),
            tool("e2", "toolu_1", false),
            text("e3", 3, "New."),
        ];
        assert_eq!(look(&mut shown, &after, &[]), ("New.\n".into(), false));
        // Two tool calls from one event are two messages; so is the same event's text.
        let two = [tool("e4", "toolu_2", false), tool("e4", "toolu_3", false)];
        assert_eq!(
            look(&mut shown, &two, &[]),
            ("  · Bash toolu_2\n  · Bash toolu_3\n".into(), false)
        );
        // A view that moved on (the oldest gone) prints only what's new, even from the same
        // event.
        let moved = [two[1].clone(), tool("e4", "toolu_4", false)];
        assert_eq!(
            look(&mut shown, &moved, &[]),
            ("  · Bash toolu_4\n".into(), false)
        );
    }
}
