//! A thread's subagents for display: each one's card, the messages that are its own (shown
//! under its card), and the rest of the thread in order. Exact from the harness's own records
//! in the VM ([`Lineage`]) once Flotilla has read them: a subagent's calls by id, what it said
//! by text; for Codex (whose subagents boat's events leave out) a card made here, with its
//! answer taken back out of the reply boat merged it into; for OpenCode, its card in place of
//! the `task` call boat shows. Until then, and for Claude Code only, inferred from boat's
//! events (see [`inferred`]).

use flotilla_model::subagent::{self, Subagent};
use flotilla_proto::{Lineage, Message, Subagent as Child};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// A subagent's card: what it was asked, how far it's got, and what it reported.
#[derive(Clone, PartialEq, Hash)]
pub struct SubCard {
    pub description: String,
    pub kind: Option<String>,
    /// What it was asked to do; empty when that can't be read (Codex keeps it encrypted).
    pub prompt: String,
    pub background: bool,
    pub report: Option<String>,
    /// How many calls it made and its latest (name, summary), when they're known to be its
    /// own: not while it runs beside others started with it and its calls are only inferred.
    pub calls: Option<usize>,
    pub latest: Option<(String, String)>,
    pub duration_ms: Option<u64>,
}

/// One message to show, in order.
pub struct Planned {
    pub msg: Message,
    pub ident: String,
    /// This is a subagent's card.
    pub card: Option<Rc<SubCard>>,
    /// The card (by place in the plan) it's shown under.
    pub owner: Option<usize>,
}

pub struct Plan {
    pub items: Vec<Planned>,
    /// The running subagents' calls together, and the latest of them.
    pub running_calls: (usize, Option<(String, String)>),
}

/// A message's ident: its event, kind and how many of those came before it in that event.
pub fn idents(messages: &[Message]) -> Vec<String> {
    let mut counts: HashMap<(String, String), usize> = HashMap::new();
    messages
        .iter()
        .map(|m| {
            let key = (m.event_key.clone().unwrap_or_default(), m.kind.clone());
            let n = counts.entry(key.clone()).or_default();
            *n += 1;
            format!("{}:{}:{}", key.0, key.1, n)
        })
        .collect()
}

/// Where a card comes from.
enum Source {
    /// A Claude Code Agent call, or an OpenCode task call, in the thread.
    Message(usize),
    /// A Codex subagent, from the VM's records.
    Child(usize),
}

struct CardPlan {
    source: Source,
    sub: SubCard,
    running: bool,
    /// Its own messages, in order.
    own: Vec<usize>,
    /// The card its group's calls are kept under (itself, unless inferred and shared).
    group: usize,
    /// Running or not from the records, put on the call it hangs off: OpenCode's task, which
    /// boat never shows ending, and a Claude Code subagent in the background, whose call
    /// returns as soon as it's launched.
    state: Option<bool>,
    /// Placed by the records rather than where boat put its call (OpenCode).
    moved: bool,
    /// Its calls are counted in the records (Codex, OpenCode), not only from boat's events.
    recorded: bool,
}

/// `working`: the thread's turn is still running (a Codex record read during it may be stale).
pub fn plan(
    messages: &[Message],
    idents: &[String],
    lineage: Option<&Lineage>,
    working: bool,
) -> Plan {
    let children: &[Child] = lineage.map(|l| l.subagents.as_slice()).unwrap_or(&[]);
    // A Claude Code or OpenCode subagent hangs off the call that started it; one whose call
    // isn't here yet is skipped.
    let card_of: HashMap<&str, usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.kind == "tool")
        .filter_map(|(i, m)| m.tool_id.as_deref().map(|id| (id, i)))
        .collect();
    let shown: Vec<bool> = children
        .iter()
        .map(|c| {
            c.parent_call
                .as_deref()
                .is_none_or(|p| card_of.contains_key(p))
        })
        .collect();
    let by_call: HashMap<&str, usize> = children
        .iter()
        .enumerate()
        .filter(|(c, _)| shown[*c])
        .flat_map(|(c, child)| child.calls.iter().map(move |id| (id.as_str(), c)))
        .collect();
    // Turns: each message's (counting prompts), and each subagent's: that of the call it hangs
    // off, or of its first call, or the last prompt sent before it started. Its words are only
    // looked for there, so a line it shares with an earlier reply stays with that reply.
    let turn_of: Vec<usize> = messages
        .iter()
        .scan(0, |n, m| {
            *n += usize::from(m.kind == "prompt");
            Some(*n)
        })
        .collect();
    let turns = turn_of.last().copied().unwrap_or(0);
    let child_turn: Vec<usize> = children
        .iter()
        .map(|child| {
            let host = child
                .parent_call
                .as_deref()
                .and_then(|p| card_of.get(p))
                .copied();
            let first = || {
                messages.iter().position(|m| {
                    m.kind == "tool"
                        && m.tool_id
                            .as_ref()
                            .is_some_and(|id| child.calls.contains(id))
                })
            };
            match host.or_else(first) {
                Some(i) => turn_of[i],
                None => match child.started_ms {
                    Some(start) => messages
                        .iter()
                        .filter(|m| m.kind == "prompt" && m.timestamp.is_some_and(|t| t <= start))
                        .count()
                        .max(1)
                        .min(turns),
                    None => turns,
                },
            }
        })
        .collect();

    // Each message's subagent, exactly; and replies with a subagent's words taken out.
    let mut child_of: Vec<Option<usize>> = vec![None; messages.len()];
    let mut texts: HashMap<usize, String> = HashMap::new();
    for (i, m) in messages.iter().enumerate() {
        match m.kind.as_str() {
            "tool" => child_of[i] = m.tool_id.as_deref().and_then(|id| by_call.get(id).copied()),
            "text" => {
                let Some(text) = m.text.as_deref() else {
                    continue;
                };
                let mut rest = text.to_string();
                let mut said_by = None;
                for (c, child) in children
                    .iter()
                    .enumerate()
                    .filter(|(c, _)| shown[*c] && child_turn[*c] == turn_of[i])
                {
                    for t in child
                        .texts
                        .iter()
                        .map(|t| t.trim())
                        .filter(|t| !t.is_empty())
                    {
                        if let Some(at) = whole_lines(&rest, t) {
                            rest.replace_range(at..at + t.len(), "");
                            said_by.get_or_insert(c);
                        }
                    }
                }
                match said_by {
                    Some(c) if rest.trim().is_empty() => child_of[i] = Some(c),
                    Some(_) => {
                        texts.insert(i, rest.trim().to_string());
                    }
                    None => {}
                }
            }
            _ => {}
        }
    }

    let tool_count = |own: &[usize]| own.iter().filter(|&&i| messages[i].kind == "tool").count();
    let latest = |own: &[usize]| {
        own.iter()
            .rev()
            .find(|&&i| messages[i].kind == "tool")
            .map(|&i| {
                (
                    messages[i].tool_name.clone().unwrap_or_default(),
                    messages[i].tool_summary.clone().unwrap_or_default(),
                )
            })
    };
    let mut cards: Vec<CardPlan> = Vec::new();
    let mut exact_cards: HashSet<usize> = HashSet::new();
    for (c, child) in children.iter().enumerate().filter(|(c, _)| shown[*c]) {
        let own: Vec<usize> = (0..messages.len())
            .filter(|&i| child_of[i] == Some(c))
            .collect();
        let took = child
            .started_ms
            .zip(child.finished_ms)
            .map(|(s, f)| (f - s).max(0) as u64);
        let host = child
            .parent_call
            .as_deref()
            .and_then(|p| card_of.get(p))
            .copied();
        match host.and_then(|card| subagent::from_message(&messages[card]).map(|s| (card, s))) {
            // Claude Code: the Agent call says most of it.
            Some((card, s)) => {
                exact_cards.insert(card);
                // In the background its call returns at once ("launched"): the records say
                // when it's done, and its own last words are its report.
                let background = s.background || child.background;
                let running = if background {
                    child.running && working
                } else {
                    messages[card].pending
                };
                let report = if background {
                    (!running).then(|| child.texts.last().cloned()).flatten()
                } else {
                    s.report
                        .clone()
                        .or_else(|| (!running).then(|| child.texts.last().cloned()).flatten())
                };
                let duration_ms = if background {
                    took
                } else {
                    s.duration_ms.or(took)
                };
                let sub = card_from(s, Some(tool_count(&own)), latest(&own), report, duration_ms);
                let state = background.then_some(running);
                cards.push(CardPlan {
                    source: Source::Message(card),
                    sub,
                    running,
                    own,
                    group: card,
                    state,
                    moved: false,
                    recorded: false,
                });
            }
            // Codex (no call to hang off) and OpenCode (a task call that says little): the records.
            None => {
                let running = child.running && working;
                let sub = SubCard {
                    description: capitalised(&child.name),
                    kind: child.kind.clone(),
                    prompt: child.prompt.clone().unwrap_or_default(),
                    background: child.background,
                    report: if running {
                        None
                    } else {
                        child.texts.last().cloned()
                    },
                    calls: Some(tool_count(&own).max(child.calls.len())),
                    latest: latest(&own).or_else(|| child.latest.clone()),
                    duration_ms: took,
                };
                let (source, state, group) = match host {
                    Some(card) => {
                        exact_cards.insert(card);
                        (Source::Message(card), Some(running), card)
                    }
                    None => (Source::Child(c), None, usize::MAX - c),
                };
                let moved = host.is_some();
                cards.push(CardPlan {
                    source,
                    sub,
                    running,
                    own,
                    group,
                    state,
                    moved,
                    recorded: true,
                });
            }
        }
    }
    // The rest inferred, from what the records don't claim.
    for (card, s, span, shared, group) in inferred(messages) {
        if exact_cards.contains(&card) {
            continue;
        }
        let own: Vec<usize> = span.filter(|&i| child_of[i].is_none()).collect();
        let running = messages[card].pending;
        // Done, it says how many calls it made; running alone, they're the ones under it.
        let calls = match s.tool_uses {
            Some(n) if !running => Some(n),
            _ if shared => None,
            _ => Some(tool_count(&own)),
        };
        let mine = if shared { None } else { latest(&own) };
        let (report, duration_ms) = (s.report.clone(), s.duration_ms);
        let sub = card_from(s, calls, mine, report, duration_ms);
        cards.push(CardPlan {
            source: Source::Message(card),
            sub,
            running,
            own,
            group,
            state: None,
            moved: false,
            recorded: false,
        });
    }

    // Lay out: each card, then its own messages; a Codex card goes where its first message was
    // (or where it started, with none yet).
    let mut owner_card: HashMap<usize, usize> = HashMap::new();
    for (k, card) in cards.iter().enumerate() {
        for &i in &card.own {
            owner_card.entry(i).or_insert(k);
        }
    }
    // When a subagent started: its card goes before what came after that in its turn (or at
    // the turn's end).
    let from_start = |started: Option<i64>, turn: usize| {
        let start = started.unwrap_or(i64::MAX);
        let end = turn_of
            .iter()
            .position(|&t| t > turn)
            .unwrap_or(messages.len());
        (0..end)
            .find(|&i| {
                turn_of[i] == turn
                    && messages[i].kind != "prompt"
                    && messages[i].timestamp.is_some_and(|t| t >= start)
            })
            .unwrap_or(end)
    };
    let mut at_message: HashMap<usize, usize> = HashMap::new();
    let mut before: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut moved: HashSet<usize> = HashSet::new();
    for (k, card) in cards.iter().enumerate() {
        match card.source {
            // OpenCode's events are stamped by when their message began, so the agent's reply
            // can come before the task call it waited for: the card goes before the texts the
            // records say came after it (or, failing that, by when the subagent started).
            Source::Message(i) if card.moved => {
                let c = children.iter().position(|ch| {
                    ch.parent_call.is_some() && ch.parent_call == messages[i].tool_id
                });
                let after: Vec<&str> = c
                    .map(|c| {
                        children[c]
                            .replies_after
                            .iter()
                            .map(|t| t.trim())
                            .filter(|t| !t.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                let turn_start = messages[..i]
                    .iter()
                    .rposition(|m| m.kind == "prompt")
                    .unwrap_or(0);
                let reply = (turn_start..i).find(|&j| {
                    messages[j].kind == "text"
                        && messages[j]
                            .text
                            .as_deref()
                            .is_some_and(|t| after.contains(&t.trim()))
                });
                let anchor = reply
                    .unwrap_or_else(|| {
                        from_start(c.and_then(|c| children[c].started_ms), turn_of[i])
                    })
                    .min(i);
                moved.insert(i);
                before.entry(anchor).or_default().push(k);
            }
            Source::Message(i) => {
                at_message.insert(i, k);
            }
            Source::Child(c) => {
                let anchor = card
                    .own
                    .first()
                    .copied()
                    .unwrap_or_else(|| from_start(children[c].started_ms, child_turn[c]));
                before.entry(anchor).or_default().push(k);
            }
        }
    }
    let mut items: Vec<Planned> = Vec::new();
    let mut place: Vec<Option<usize>> = vec![None; cards.len()];
    let emit_own = |k: usize, items: &mut Vec<Planned>, place: &[Option<usize>]| {
        for &i in &cards[k].own {
            let mut msg = messages[i].clone();
            if let Some(t) = texts.get(&i) {
                msg.text = Some(t.clone());
            }
            items.push(Planned {
                msg,
                ident: idents[i].clone(),
                card: None,
                owner: place[k],
            });
        }
    };
    for i in 0..=messages.len() {
        for &k in before.get(&i).into_iter().flatten() {
            let c = match cards[k].source {
                Source::Child(c) => c,
                // A moved call: itself, with its state from the records.
                Source::Message(host) => {
                    let mut msg = messages[host].clone();
                    msg.pending = cards[k].running;
                    msg.status = None;
                    msg.is_error = false;
                    place[k] = Some(items.len());
                    items.push(Planned {
                        msg,
                        ident: idents[host].clone(),
                        card: Some(Rc::new(cards[k].sub.clone())),
                        owner: None,
                    });
                    emit_own(k, &mut items, &place);
                    continue;
                }
            };
            let msg = Message {
                kind: "tool".into(),
                tool_name: Some("Subagent".into()),
                pending: cards[k].running,
                task_id: messages
                    .get(cards[k].own.first().copied().unwrap_or(i))
                    .and_then(|m| m.task_id.clone()),
                timestamp: children[c].started_ms,
                event_key: Some(format!("sub-{}", children[c].id)),
                ..Default::default()
            };
            place[k] = Some(items.len());
            items.push(Planned {
                msg,
                ident: format!("sub-{}:card", children[c].id),
                card: Some(Rc::new(cards[k].sub.clone())),
                owner: None,
            });
            emit_own(k, &mut items, &place);
        }
        if i == messages.len() {
            break;
        }
        if owner_card.contains_key(&i) || moved.contains(&i) {
            continue;
        }
        let mut msg = messages[i].clone();
        if let Some(t) = texts.get(&i) {
            msg.text = Some(t.clone());
        }
        let card = at_message.get(&i).copied();
        if let Some(k) = card {
            place[k] = Some(items.len());
            if let Some(running) = cards[k].state {
                msg.pending = running;
                msg.status = None;
            }
        }
        items.push(Planned {
            msg,
            ident: idents[i].clone(),
            card: card.map(|k| Rc::new(cards[k].sub.clone())),
            owner: None,
        });
        if let Some(k) = card {
            emit_own(k, &mut items, &place);
        }
    }

    // The running subagents' calls, each group once (inferred groups share theirs).
    let running_groups: HashSet<usize> = cards
        .iter()
        .filter(|c| c.running)
        .map(|c| c.group)
        .collect();
    let mut total = 0;
    let mut newest: Option<(usize, String, String)> = None;
    let mut recorded_latest = None;
    for card in cards.iter().filter(|c| running_groups.contains(&c.group)) {
        // The records count calls boat doesn't show (OpenCode's); inferred groups share theirs.
        total += if card.recorded {
            card.sub.calls.unwrap_or(0)
        } else {
            tool_count(&card.own)
        };
        match card.own.iter().rev().find(|&&i| messages[i].kind == "tool") {
            Some(&i) if newest.as_ref().is_none_or(|(n, _, _)| i > *n) => {
                newest = Some((
                    i,
                    messages[i].tool_name.clone().unwrap_or_default(),
                    messages[i].tool_summary.clone().unwrap_or_default(),
                ));
            }
            Some(_) => {}
            None if card.running => recorded_latest = card.sub.latest.clone().or(recorded_latest),
            None => {}
        }
    }
    let latest = newest
        .map(|(_, name, what)| (name, what))
        .or(recorded_latest);
    Plan {
        items,
        running_calls: (total, latest),
    }
}

/// Where `said` stands in `text` as whole lines (boat puts a subagent's words in the reply that
/// way), so a short answer isn't cut out of the middle of the agent's own sentence.
fn whole_lines(text: &str, said: &str) -> Option<usize> {
    text.match_indices(said).map(|(at, _)| at).find(|&at| {
        let before = text[..at].chars().next_back().is_none_or(|c| c == '\n');
        let after = text[at + said.len()..]
            .chars()
            .next()
            .is_none_or(|c| c == '\n');
        before && after
    })
}

fn card_from(
    s: Subagent,
    calls: Option<usize>,
    latest: Option<(String, String)>,
    report: Option<String>,
    duration_ms: Option<u64>,
) -> SubCard {
    SubCard {
        description: s.description,
        kind: s.kind,
        prompt: s.prompt,
        background: s.background,
        report,
        calls,
        latest,
        duration_ms,
    }
}

fn capitalised(name: &str) -> String {
    let mut chars = name.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Subagent".into(),
    }
}

/// A subagent call's card, its own messages, whether it shares them, and its group.
pub type Span = (usize, Subagent, std::ops::Range<usize>, bool, usize);

/// Each Claude Code subagent call (by index into `messages`), and the messages after it that
/// are its own, inferred from boat's events: from the end of the event that started it (the
/// agent's other calls in it are its own) to the end of the turn while it runs (the thread's
/// agent waits for it) or after a cut-short turn, else as many tool calls as it reports making.
/// Subagents started together share one run of messages, kept under the last one waited for.
/// One in the background keeps none: the thread's agent carried on meanwhile.
/// Also, for each: whether it was started with others waited for (so its calls can't be told
/// from theirs), and its group (the card its group's calls are kept under).
pub fn inferred(messages: &[Message]) -> Vec<Span> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < messages.len() {
        let Some(first) = subagent::from_message(&messages[i]) else {
            i += 1;
            continue;
        };
        let mut group = vec![(i, first)];
        let mut next = i + 1;
        while messages[i].event_key.is_some()
            && next < messages.len()
            && messages[next].event_key == messages[i].event_key
        {
            if let Some(s) = subagent::from_message(&messages[next]) {
                group.push((next, s));
            }
            next += 1;
        }
        let turn_end = messages[next..]
            .iter()
            .position(|m| m.kind == "prompt")
            .map_or(messages.len(), |p| next + p);
        let waited: Vec<usize> = group
            .iter()
            .filter(|(_, s)| !s.background)
            .map(|(i, _)| *i)
            .collect();
        let end = if waited.is_empty() {
            next
        } else if waited
            .iter()
            .any(|&c| messages[c].pending || messages[c].status.is_some())
        {
            turn_end
        } else {
            let mut left: usize = group
                .iter()
                .filter(|(_, s)| !s.background)
                .filter_map(|(_, s)| s.tool_uses)
                .sum();
            let mut end = next;
            while left > 0 && end < turn_end {
                if messages[end].kind == "tool" {
                    left -= 1;
                }
                end += 1;
            }
            end
        };
        let keeper = waited.last().copied();
        let shared = waited.len() > 1;
        let group_key = keeper.unwrap_or(i);
        for (card, s) in group {
            let span = if Some(card) == keeper {
                next..end
            } else {
                next..next
            };
            let shared = shared && !s.background;
            out.push((card, s, span, shared, group_key));
        }
        i = end.max(next);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(kind: &str, event: &str) -> Message {
        Message {
            kind: kind.into(),
            event_key: Some(event.into()),
            ..Default::default()
        }
    }

    fn tool(event: &str, id: &str) -> Message {
        Message {
            tool_name: Some("Bash".into()),
            tool_id: Some(id.into()),
            tool_summary: Some(format!("run {id}")),
            ..msg("tool", event)
        }
    }

    fn text(event: &str, t: &str) -> Message {
        Message {
            text: Some(t.into()),
            ..msg("text", event)
        }
    }

    /// A subagent call from event `event`: running, or done with `uses` calls.
    fn agent(event: &str, uses: Option<usize>, background: bool) -> Message {
        let output = uses
            .map(|n| format!("Done.\nagentId: a1\n<usage>tool_uses: {n}\nduration_ms: 900</usage>"))
            .unwrap_or_default();
        Message {
            tool_name: Some("Agent".into()),
            tool_input_json: Some(serde_json::json!({"description": "Look", "prompt": "Look around.", "run_in_background": background}).to_string()),
            tool_result_json: Some(serde_json::json!({"type": "generic_tool", "output": output}).to_string()),
            pending: uses.is_none(),
            ..msg("tool", event)
        }
    }

    fn spans(messages: &[Message]) -> Vec<(usize, std::ops::Range<usize>)> {
        inferred(messages)
            .into_iter()
            .map(|(card, _, span, _, _)| (card, span))
            .collect()
    }

    /// The plan as (ident, owner) pairs, cards marked with a `*`.
    fn shape(
        messages: &[Message],
        lineage: Option<&Lineage>,
        working: bool,
    ) -> Vec<(String, Option<usize>)> {
        let ids = idents(messages);
        plan(messages, &ids, lineage, working)
            .items
            .into_iter()
            .map(|p| {
                (
                    format!("{}{}", if p.card.is_some() { "*" } else { "" }, p.ident),
                    p.owner,
                )
            })
            .collect()
    }

    #[test]
    fn a_subagents_calls_are_the_ones_it_made() {
        let tool = |e: &str| msg("tool", e);
        // Done after 2 calls: those two (and the text between), not the agent's own call after.
        let done = [
            msg("prompt", "p"),
            agent("e1", Some(2), false),
            tool("e2"),
            msg("text", "e3"),
            tool("e4"),
            tool("e5"),
            msg("text", "e6"),
        ];
        assert_eq!(spans(&done), [(1, 2..5)]);
        // Running: everything after it in the turn (the agent waits), not the next turn.
        let running = [
            msg("prompt", "p"),
            agent("e1", None, false),
            tool("e2"),
            tool("e3"),
            msg("prompt", "q"),
            tool("e4"),
        ];
        assert_eq!(spans(&running), [(1, 2..4)]);
        // In the background: none (the agent carried on).
        let background = [
            msg("prompt", "p"),
            agent("e1", Some(2), true),
            tool("e2"),
            tool("e3"),
        ];
        assert_eq!(spans(&background), [(1, 2..2)]);
        // Two at once: their calls together, under the last.
        let pair = [
            msg("prompt", "p"),
            agent("e1", Some(1), false),
            agent("e1", Some(2), false),
            tool("e2"),
            tool("e3"),
            tool("e4"),
            tool("e5"),
        ];
        assert_eq!(spans(&pair), [(1, 3..3), (2, 3..6)]);
        // The agent's own call beside it stays its own; the subagent's start after that event.
        let beside = [
            msg("prompt", "p"),
            agent("e1", Some(1), false),
            tool("e1"),
            tool("e2"),
            tool("e3"),
        ];
        assert_eq!(spans(&beside), [(1, 3..4)]);
        // Running, with a call between two started together: both are cards, the call isn't theirs.
        let between = [
            msg("prompt", "p"),
            agent("e1", None, false),
            tool("e1"),
            agent("e1", None, false),
            tool("e2"),
        ];
        assert_eq!(spans(&between), [(1, 4..4), (3, 4..5)]);
        // One waited for and one in the background: the calls go under the one waited for.
        let mixed = [
            msg("prompt", "p"),
            agent("e1", Some(1), false),
            agent("e1", Some(5), true),
            tool("e2"),
            tool("e3"),
        ];
        assert_eq!(spans(&mixed), [(1, 3..4), (2, 3..3)]);
        // Three started together, the one keeping their calls done first: the group's calls
        // still count while the others run.
        let mut trio = vec![
            msg("prompt", "p"),
            agent("e1", None, false),
            agent("e1", None, false),
            agent("e1", Some(1), false),
            tool("e2"),
            tool("e3"),
        ];
        trio[3].pending = false;
        let groups: Vec<usize> = inferred(&trio)
            .into_iter()
            .map(|(_, _, _, _, g)| g)
            .collect();
        assert_eq!(groups, [3, 3, 3]);
        assert_eq!(spans(&trio), [(1, 4..4), (2, 4..4), (3, 4..6)]);
        // Cut short (the turn was stopped): the rest of the turn.
        let mut stopped = agent("e1", None, false);
        stopped.pending = false;
        stopped.status = Some("interrupted".into());
        assert_eq!(
            spans(&[msg("prompt", "p"), stopped, tool("e2")]),
            [(1, 2..3)]
        );
    }

    #[test]
    fn claude_code_subagents_get_their_own_calls_exactly() {
        // Two started together; their calls interleave, and the agent made one of its own.
        let mut a = agent("e1", Some(1), false);
        a.tool_id = Some("toolu_a".into());
        let mut b = agent("e1", Some(1), false);
        b.tool_id = Some("toolu_b".into());
        let messages = [
            msg("prompt", "p"),
            a,
            b,
            tool("e2", "toolu_b1"),
            tool("e3", "toolu_a1"),
            tool("e4", "toolu_own"),
        ];
        let child = |id: &str, call: &str, calls: &[&str]| Child {
            id: id.into(),
            parent_call: Some(call.into()),
            name: id.into(),
            calls: calls.iter().map(|c| c.to_string()).collect(),
            ..Default::default()
        };
        let lineage = Lineage {
            subagents: vec![
                child("a", "toolu_a", &["toolu_a1"]),
                child("b", "toolu_b", &["toolu_b1"]),
            ],
        };
        let s = shape(&messages, Some(&lineage), false);
        let names: Vec<&str> = s.iter().map(|(i, _)| i.as_str()).collect();
        assert_eq!(
            names,
            [
                "p:prompt:1",
                "*e1:tool:1",
                "e3:tool:1",
                "*e1:tool:2",
                "e2:tool:1",
                "e4:tool:1"
            ]
        );
        assert_eq!(
            s.iter().map(|(_, o)| *o).collect::<Vec<_>>(),
            [None, None, Some(1), None, Some(3), None]
        );
    }

    #[test]
    fn codex_subagents_get_a_card_and_their_words_back() {
        // boat shows the child's call and answer as the thread's, then merges the answer into
        // the reply.
        let messages = [
            msg("prompt", "p"),
            text("e1", "uname: Linux"),
            tool("e1", "exec-1"),
            text("e2", "uname: Linux\nThe machine runs Linux."),
        ];
        let lineage = Lineage {
            subagents: vec![Child {
                id: "k1".into(),
                name: "system_info".into(),
                kind: Some("Hypatia".into()),
                calls: vec!["exec-1".into()],
                texts: vec!["uname: Linux".into()],
                started_ms: Some(1),
                finished_ms: Some(2001),
                ..Default::default()
            }],
        };
        let ids = idents(&messages);
        let p = plan(&messages, &ids, Some(&lineage), false);
        let shape: Vec<(String, Option<usize>)> =
            p.items.iter().map(|i| (i.ident.clone(), i.owner)).collect();
        assert_eq!(
            shape,
            [
                ("p:prompt:1".into(), None),
                ("sub-k1:card".into(), None),
                ("e1:text:1".into(), Some(1)),
                ("e1:tool:1".into(), Some(1)),
                ("e2:text:1".into(), None)
            ]
        );
        let card = p.items[1].card.as_ref().unwrap();
        assert_eq!(
            (
                card.description.as_str(),
                card.calls,
                card.report.as_deref()
            ),
            ("System_info", Some(1), Some("uname: Linux"))
        );
        assert_eq!(
            p.items[4].msg.text.as_deref(),
            Some("The machine runs Linux.")
        );
        // Only whole lines: the agent's own sentence that happens to contain the words stays.
        assert_eq!(
            whole_lines("uname: Linux\nThe end.", "uname: Linux"),
            Some(0)
        );
        assert_eq!(whole_lines("I ran it. Done.\nDone.", "Done."), Some(16));
        assert_eq!(whole_lines("I ran it. Done.", "Done."), None);
        // A record read while the turn ran says it's running; once the turn's over, it isn't.
        let mut running = lineage.clone();
        running.subagents[0].running = true;
        assert!(
            plan(&messages, &ids, Some(&running), true).items[1]
                .msg
                .pending
        );
        assert!(
            !plan(&messages, &ids, Some(&running), false).items[1]
                .msg
                .pending
        );
    }

    #[test]
    fn opencode_subagents_take_the_place_of_their_task_call() {
        // boat shows a task call with a placeholder input that never ends, and none of its calls.
        let mut task = tool("e1", "call_task");
        task.tool_name = Some("task".into());
        task.status = Some("unfinished".into());
        task.timestamp = Some(30);
        // boat stamps the reply by when its message began, before the subagent even started.
        let mut reply = text("e2", "Linux, 29% used.");
        reply.timestamp = Some(5);
        let messages = [msg("prompt", "p"), reply, task];
        let lineage = Lineage {
            subagents: vec![Child {
                id: "ses_child".into(),
                parent_call: Some("call_task".into()),
                name: "Inspect system details".into(),
                kind: Some("general".into()),
                prompt: Some("Run uname -a and df -h /.".into()),
                calls: vec!["call_1".into(), "call_2".into()],
                latest: Some(("bash".into(), "df -h /".into())),
                texts: vec!["Linux 6.8 (x86_64); 29% used.".into()],
                started_ms: Some(10),
                finished_ms: Some(25),
                replies_after: vec!["Linux, 29% used.".into()],
                ..Default::default()
            }],
        };
        let ids = idents(&messages);
        let p = plan(&messages, &ids, Some(&lineage), false);
        assert_eq!(
            p.items.iter().map(|i| i.ident.as_str()).collect::<Vec<_>>(),
            ["p:prompt:1", "e1:tool:1", "e2:text:1"]
        );
        let card = p.items[1].card.as_ref().unwrap();
        assert_eq!(
            (card.description.as_str(), card.calls, card.prompt.as_str()),
            (
                "Inspect system details",
                Some(2),
                "Run uname -a and df -h /."
            )
        );
        assert_eq!(card.latest, Some(("bash".into(), "df -h /".into())));
        assert_eq!(
            (p.items[1].msg.pending, p.items[1].msg.status.as_deref()),
            (false, None)
        );
    }

    #[test]
    fn a_subagents_words_are_only_looked_for_in_its_turn() {
        // The agent said "Done." in the first turn; a Codex subagent says it in the second.
        let mut p1 = msg("prompt", "p1");
        p1.timestamp = Some(1);
        let mut p2 = msg("prompt", "p2");
        p2.timestamp = Some(100);
        let messages = [
            p1,
            text("e1", "Done."),
            p2,
            tool("e2", "exec-2"),
            text("e3", "Done.\nAll good."),
        ];
        let lineage = Lineage {
            subagents: vec![Child {
                id: "k".into(),
                name: "check".into(),
                calls: vec!["exec-2".into()],
                texts: vec!["Done.".into()],
                started_ms: Some(101),
                ..Default::default()
            }],
        };
        let ids = idents(&messages);
        let p = plan(&messages, &ids, Some(&lineage), false);
        let shape: Vec<(&str, Option<usize>, Option<&str>)> = p
            .items
            .iter()
            .map(|i| (i.ident.as_str(), i.owner, i.msg.text.as_deref()))
            .collect();
        assert_eq!(
            shape,
            [
                ("p1:prompt:1", None, None),
                ("e1:text:1", None, Some("Done.")),
                ("p2:prompt:1", None, None),
                ("sub-k:card", None, None),
                ("e2:tool:1", Some(3), None),
                ("e3:text:1", None, Some("All good."))
            ]
        );
    }

    #[test]
    fn a_subagent_without_calls_yet_is_placed_by_its_start_time() {
        // The second prompt went at 2.1 s; a Codex subagent started at 2.5 s, before any call.
        let mut p1 = msg("prompt", "p1");
        p1.timestamp = Some(1000);
        let mut p2 = msg("prompt", "p2");
        p2.timestamp = Some(2100);
        let mut reply = text("e1", "Done.");
        reply.timestamp = Some(1500);
        let mut merged = text("e3", "Done.\nAll good.");
        merged.timestamp = Some(3000);
        let messages = [p1, reply, p2, merged];
        let lineage = Lineage {
            subagents: vec![Child {
                id: "k".into(),
                name: "check".into(),
                texts: vec!["Done.".into()],
                started_ms: Some(2500),
                ..Default::default()
            }],
        };
        let ids = idents(&messages);
        let p = plan(&messages, &ids, Some(&lineage), false);
        let shape: Vec<(&str, Option<&str>)> = p
            .items
            .iter()
            .map(|i| (i.ident.as_str(), i.msg.text.as_deref()))
            .collect();
        assert_eq!(
            shape,
            [
                ("p1:prompt:1", None),
                ("e1:text:1", Some("Done.")),
                ("p2:prompt:1", None),
                ("sub-k:card", None),
                ("e3:text:1", Some("All good."))
            ]
        );
    }

    #[test]
    fn a_background_subagent_runs_until_the_records_say_its_done() {
        // Its call returned at once with "launched"; the subagent is still going.
        let mut launch = agent("e1", Some(0), true);
        launch.tool_id = Some("toolu_bg".into());
        launch.tool_result_json = Some(serde_json::json!({"type": "generic_tool", "output": "Async agent launched successfully."}).to_string());
        let messages = [
            msg("prompt", "p"),
            launch,
            text("e2", "Meanwhile, I'll keep going."),
        ];
        let mut child = Child {
            id: "bg".into(),
            parent_call: Some("toolu_bg".into()),
            background: true,
            running: true,
            texts: vec!["Found 3 call sites.".into()],
            ..Default::default()
        };
        let ids = idents(&messages);
        let running = plan(
            &messages,
            &ids,
            Some(&Lineage {
                subagents: vec![child.clone()],
            }),
            true,
        );
        assert!(running.items[1].msg.pending);
        assert_eq!(running.items[1].card.as_ref().unwrap().report, None);
        child.running = false;
        let done = plan(
            &messages,
            &ids,
            Some(&Lineage {
                subagents: vec![child],
            }),
            true,
        );
        assert!(!done.items[1].msg.pending);
        assert_eq!(
            done.items[1].card.as_ref().unwrap().report.as_deref(),
            Some("Found 3 call sites.")
        );
    }

    #[test]
    fn the_folded_line_counts_calls_boat_doesnt_show() {
        // Two OpenCode subagents running: their calls are only in the records.
        let task = |e: &str, id: &str| Message {
            tool_name: Some("task".into()),
            ..tool(e, id)
        };
        let messages = [
            msg("prompt", "p"),
            task("e1", "call_a"),
            task("e1", "call_b"),
        ];
        let child = |id: &str, call: &str, calls: usize, latest: &str| Child {
            id: id.into(),
            parent_call: Some(call.into()),
            running: true,
            calls: (0..calls).map(|n| format!("{id}{n}")).collect(),
            latest: Some(("bash".into(), latest.into())),
            ..Default::default()
        };
        let lineage = Lineage {
            subagents: vec![
                child("a", "call_a", 2, "ls"),
                child("b", "call_b", 3, "sleep 9"),
            ],
        };
        let ids = idents(&messages);
        let p = plan(&messages, &ids, Some(&lineage), true);
        assert_eq!(p.running_calls.0, 5);
        assert!(p.running_calls.1.is_some());
    }

    #[test]
    fn without_records_nothing_changes_for_other_threads() {
        let messages = [
            msg("prompt", "p"),
            text("e1", "No subagent tool here."),
            tool("e2", "call_1"),
        ];
        let s = shape(&messages, None, false);
        assert_eq!(
            s,
            [
                ("p:prompt:1".into(), None),
                ("e1:text:1".into(), None),
                ("e2:tool:1".into(), None)
            ]
        );
        assert_eq!(shape(&messages, Some(&Lineage::default()), false), s);
    }
}
