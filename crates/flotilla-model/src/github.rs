//! What a client works out about GitHub from what it was sent (`flotilla_core::github` asks
//! GitHub and gh).

/// One thing a reviewer asked for: a review's own text (no path), or the first comment of
/// an unresolved thread on a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewNote {
    pub author: String,
    pub path: Option<String>,
    pub line: Option<i64>,
    pub body: String,
}

/// The pull request number in what someone typed: `#12`, or a PR URL in `owner/name`
/// (`https://github.com/owner/name/pull/12`, any tab of it). `None`: not a PR reference. A
/// URL for another repo is an error that says so.
pub fn parse_pr_ref(input: &str, owner: &str, name: &str) -> Option<Result<i64, String>> {
    let s = input.trim();
    if let Some(n) = s.strip_prefix('#') {
        return n.parse::<i64>().ok().filter(|n| *n > 0).map(Ok);
    }
    let path = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let path = path
        .strip_prefix("www.")
        .unwrap_or(path)
        .strip_prefix("github.com/")?;
    let mut parts = path.split(['/', '?', '#']);
    let (o, r, pull, num) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if pull != "pull" {
        return None;
    }
    let num = num.parse::<i64>().ok().filter(|n| *n > 0)?;
    if !o.eq_ignore_ascii_case(owner) || !r.eq_ignore_ascii_case(name) {
        return Some(Err(format!(
            "That pull request is in {o}/{r}, not {owner}/{name}."
        )));
    }
    Some(Ok(num))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_request_references() {
        let pr = |s: &str| parse_pr_ref(s, "acme", "flotilla-playground");
        assert_eq!(pr("#12"), Some(Ok(12)));
        assert_eq!(pr(" #7 "), Some(Ok(7)));
        assert_eq!(
            pr("https://github.com/acme/flotilla-playground/pull/3"),
            Some(Ok(3))
        );
        assert_eq!(
            pr("github.com/Acme/Flotilla-Playground/pull/3/files?w=1"),
            Some(Ok(3))
        );
        assert_eq!(
            pr("https://github.com/acme/flotilla-playground/pull/3#discussion_r1"),
            Some(Ok(3))
        );
        assert_eq!(
            pr("https://github.com/acme/web/pull/9"),
            Some(Err(
                "That pull request is in acme/web, not acme/flotilla-playground.".into()
            ))
        );
        assert_eq!(
            pr("https://github.com/acme/flotilla-playground/issues/3"),
            None
        );
        assert_eq!(pr("#0"), None);
        assert_eq!(pr("#x"), None);
        assert_eq!(pr("fix-login"), None);
    }
}
