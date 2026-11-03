//! A question an agent asked you with its harness's ask tool, as its thread shows it: a
//! message of kind [`KIND`]. The questions are its `tool_input_json` (a list of [`Question`]),
//! your answers, once given, its `tool_result_json` (one list of strings per question), and
//! its `status` says where it stands ([`WAITING`], [`ANSWERED`] or [`PASSED`]).

use crate::Message;
use serde::{Deserialize, Serialize};

pub const KIND: &str = "ask";
/// Not answered yet, and nothing was sent in the thread since.
pub const WAITING: &str = "waiting";
pub const ANSWERED: &str = "answered";
/// The thread moved on without an answer: something else was sent after it.
pub const PASSED: &str = "passed";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// A short label above the question, where the tool gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    pub question: String,
    /// In the tool's order: the recommended one first.
    #[serde(default)]
    pub options: Vec<Choice>,
    /// Several options can be picked.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multi: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// An ask message's questions (none if it isn't one, or they don't read).
pub fn questions(m: &Message) -> Vec<Question> {
    if m.kind != KIND {
        return Vec::new();
    }
    m.tool_input_json
        .as_deref()
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or_default()
}

/// An ask message's answers, once given: one list per question.
pub fn answers(m: &Message) -> Option<Vec<Vec<String>>> {
    if m.kind != KIND {
        return None;
    }
    m.tool_result_json
        .as_deref()
        .and_then(|j| serde_json::from_str(j).ok())
}

/// Each question's answer as it's sent: blanks dropped, trimmed. An error says what's missing
/// (the questions need one answer each; a single-choice question takes one).
pub fn tidy(questions: &[Question], answers: &[Vec<String>]) -> Result<Vec<Vec<String>>, String> {
    if answers.len() != questions.len() {
        return Err("Answer each question".into());
    }
    questions
        .iter()
        .zip(answers)
        .map(|(q, a)| {
            let a: Vec<String> = a
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            match a.len() {
                0 => Err("Answer each question".to_string()),
                n if n > 1 && !q.multi => Err("Pick one answer for each question".to_string()),
                _ => Ok(a),
            }
        })
        .collect()
}

/// What the agent is sent for your answers (already [`tidy`]): each question, then its answer
/// on the next line (the options picked, joined with ", "), a blank line between questions.
pub fn reply_text(questions: &[Question], answers: &[Vec<String>]) -> String {
    questions
        .iter()
        .zip(answers)
        .map(|(q, a)| format!("{}\n{}", q.question.trim(), a.join(", ")))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(question: &str, options: &[&str], multi: bool) -> Question {
        Question {
            header: None,
            question: question.into(),
            options: options
                .iter()
                .map(|l| Choice {
                    label: l.to_string(),
                    description: None,
                })
                .collect(),
            multi,
        }
    }

    #[test]
    fn answers_read_back_and_go_as_text() {
        let qs = [
            q(
                "Which file name should I create?",
                &["alpha.txt (recommended)", "beta.txt"],
                false,
            ),
            q("Which checks?", &["lint", "test"], true),
        ];
        let tidied = tidy(
            &qs,
            &[
                vec![" beta.txt ".into()],
                vec!["lint".into(), "".into(), "test".into()],
            ],
        )
        .unwrap();
        assert_eq!(tidied, [vec!["beta.txt"], vec!["lint", "test"]]);
        assert_eq!(
            reply_text(&qs, &tidied),
            "Which file name should I create?\nbeta.txt\n\nWhich checks?\nlint, test"
        );
        let m = Message {
            kind: KIND.into(),
            tool_input_json: Some(serde_json::to_string(&qs).unwrap()),
            tool_result_json: Some(serde_json::to_string(&tidied).unwrap()),
            ..Default::default()
        };
        assert_eq!(questions(&m), qs);
        assert_eq!(answers(&m), Some(tidied));
        // Only the fields that are set go on the wire.
        assert_eq!(
            serde_json::to_string(&qs[0]).unwrap(),
            r#"{"question":"Which file name should I create?","options":[{"label":"alpha.txt (recommended)"},{"label":"beta.txt"}]}"#
        );
    }

    /// What the iPhone sends (`ios/FlotillaKit`'s `AskTests`) is what this reads.
    #[test]
    fn the_answer_request_on_the_wire() {
        let req = crate::ClientMsg::Req {
            id: 7,
            req: crate::Request::Answer {
                thread: "t1".into(),
                ask: "call_1".into(),
                answers: vec![vec!["beta.txt".into()], vec!["lint".into(), "test".into()]],
                local: "local-2".into(),
            },
        };
        let swift = r#"{"id":7,"m":"thread.answer","p":{"answers":[["beta.txt"],["lint","test"]],"ask":"call_1","local":"local-2","thread":"t1"},"t":"req"}"#;
        assert_eq!(
            serde_json::to_value(&req).unwrap(),
            serde_json::from_str::<serde_json::Value>(swift).unwrap()
        );
        assert_eq!(
            serde_json::from_str::<crate::ClientMsg>(swift).unwrap(),
            req
        );
    }

    /// The message that took an answer, as the iPhone reads it (`ios/FlotillaKit`'s
    /// `AskTests`): `answer_to` only where it's set.
    #[test]
    fn the_message_that_took_an_answer_on_the_wire() {
        let m = Message {
            seq: 4,
            kind: "prompt".into(),
            task_id: Some("pr-2".into()),
            text: Some("Which file name should I create?\nbeta.txt".into()),
            status: Some("running".into()),
            answer_to: Some("call_1".into()),
            ..Default::default()
        };
        let swift = r#"{"attachments":[],"answer_to":"call_1","event_key":null,"is_error":false,"kind":"prompt","model":null,"pending":false,"seq":4,"status":"running","task_id":"pr-2","text":"Which file name should I create?\nbeta.txt","timestamp":null,"tool_id":null,"tool_input_json":null,"tool_name":null,"tool_result_json":null,"tool_result_type":null,"tool_summary":null}"#;
        assert_eq!(
            serde_json::to_value(&m).unwrap(),
            serde_json::from_str::<serde_json::Value>(swift).unwrap()
        );
        assert_eq!(serde_json::from_str::<Message>(swift).unwrap(), m);
        assert!(
            !serde_json::to_string(&Message {
                answer_to: None,
                ..m
            })
            .unwrap()
            .contains("answer_to")
        );
    }

    #[test]
    fn every_question_needs_an_answer_and_single_choice_takes_one() {
        let qs = [q("A?", &["x", "y"], false), q("B?", &[], false)];
        assert!(tidy(&qs, &[vec!["x".into()]]).is_err());
        assert!(tidy(&qs, &[vec!["x".into()], vec!["  ".into()]]).is_err());
        assert!(tidy(&qs, &[vec!["x".into(), "y".into()], vec!["typed".into()]]).is_err());
        assert_eq!(
            tidy(&qs, &[vec!["x".into()], vec!["my own".into()]]).unwrap(),
            [vec!["x"], vec!["my own"]]
        );
    }
}
