//! Agents started in a thread's own VM (`flotilla agent`): the message that sends the thread
//! all their answers, read back (`flotilla_core::side` follows them and writes it).

/// A batch's message (`flotilla_core::side::report`) read back: each agent's title and id, in order.
pub fn parse_report(text: &str) -> Option<Vec<(&str, &str)>> {
    let rest = text.strip_prefix("[flotilla] Agents answered (")?;
    let mut out = Vec::new();
    // By its headings, wherever they fall (a preview has it on one line).
    for part in rest.split("### \"").skip(1) {
        let Some((title, after)) = part.split_once("\" (") else {
            continue;
        };
        let Some((id, _)) = after.split_once(')') else {
            continue;
        };
        out.push((title, id));
    }
    Some(out)
}

/// A batch's message as it reads to you: each agent's title (bold) and how its turn ended,
/// then its answer; without the marker and the ids, which are for the agent.
pub fn report_text(text: &str) -> Option<String> {
    let members = parse_report(text)?;
    let mut body = text
        .split_once("):")
        .map_or(text, |(_, rest)| rest)
        .trim()
        .to_string();
    for (title, id) in &members {
        body = body.replacen(
            &format!("### \"{title}\" ({id}) "),
            &format!("**{title}** "),
            1,
        );
    }
    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_reads_back_on_one_line_or_several() {
        // As `flotilla_core::side::report` writes one (its test checks they agree).
        let text = "[flotilla] Agents answered (2):\n\n### \"Review a\" (a) finished:\n\nFine. 9/10\n\n### \"Review b\" (b) failed without an answer: no key for that model.";
        assert_eq!(
            parse_report(text),
            Some(vec![("Review a", "a"), ("Review b", "b")])
        );
        assert_eq!(
            parse_report(&crate::util::one_line(text)),
            Some(vec![("Review a", "a"), ("Review b", "b")])
        );
        assert_eq!(parse_report("[flotilla] \"X\" (c1) finished."), None);
        assert_eq!(
            report_text(text).unwrap(),
            "**Review a** finished:\n\nFine. 9/10\n\n**Review b** failed without an answer: no key for that model."
        );
        assert_eq!(report_text("[flotilla] \"X\" (c1) finished."), None);
    }
}
