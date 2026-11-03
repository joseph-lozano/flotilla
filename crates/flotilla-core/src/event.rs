//! boat agent events as the sync engine sees them.

use base64::Engine;

pub use flotilla_proto::RawEvent;

/// Prompt statuses after which nothing more happens in the turn.
pub fn is_terminal_status(status: &str) -> bool {
    matches!(status, "finished" | "failed" | "interrupted")
}

/// boat's cursor: base64url, no padding, of `"<timestamp>:<id>"`.
pub fn encode_cursor(timestamp: i64, id: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{timestamp}:{id}"))
}

pub fn decode_cursor(cursor: &str) -> Option<(i64, String)> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()?;
    let s = String::from_utf8(bytes).ok()?;
    let (ts, id) = s.split_once(':')?;
    Some((ts.parse().ok()?, id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trips() {
        let c = encode_cursor(1793286602665, "6b6d0659-0cd7-4113-b4af-62a5a6db5805");
        assert!(!c.contains('='));
        assert_eq!(
            decode_cursor(&c),
            Some((1793286602665, "6b6d0659-0cd7-4113-b4af-62a5a6db5805".into()))
        );
    }

    #[test]
    fn prompt_files_keep_names_not_data() {
        let e = RawEvent::from_value(serde_json::json!({
            "id": "e1", "type": "prompt", "timestamp": 1, "taskId": "t1",
            "data": {"prompt": "look", "status": "running", "attachments": [
                {"filename": "red.png", "mimeType": "image/png", "base64Data": "iVBORw0K"},
                {"filename": "note.txt", "mimeType": "text/plain", "base64Data": "aGk="}]}
        }));
        assert_eq!(e.attachment_names(), ["red.png", "note.txt"]);
        assert!(!e.raw.to_string().contains("base64Data"));
        assert!(!e.data.to_string().contains("iVBORw0K"));
        assert_eq!(
            e.data["attachments"][0],
            serde_json::json!({"filename": "red.png", "mimeType": "image/png"})
        );
    }

    #[test]
    fn prompt_events_key_by_task() {
        let e = RawEvent::from_value(serde_json::json!({
            "id": "qp-t1", "type": "prompt", "timestamp": 1, "taskId": "t1", "data": {"status": "queued"}
        }));
        assert_eq!(e.key(), "t1");
        assert!(e.is_queued_placeholder());
        let r = RawEvent::from_value(serde_json::json!({
            "id": "r1", "type": "response", "timestamp": 2, "taskId": "t1", "data": {"is_streaming": true}
        }));
        assert_eq!(r.key(), "r1");
        assert!(r.is_partial());
    }
}
