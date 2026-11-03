//! Pairing: the one-time codes, and the links that carry them.
//!
//! A link is `flotilla://pair?u=<the server's URL, percent-encoded>#<code>` (`flotilla-dev://`
//! from a dev build). The code is in the fragment, so it never reaches a server's logs or a
//! `Referer`. A server's URL is `http(s)://host[:port]` and nothing more.

/// What a code is made of: no 0, O, 1 or I to misread.
pub const CODE_ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
pub const CODE_LEN: usize = 12;
pub const SCHEME: &str = "flotilla";
pub const DEV_SCHEME: &str = "flotilla-dev";

/// What a paired device may do, kept with its token on the server and checked there on every
/// request, view and upload. A pairing code can carry one (`flotilla pair --agent`), and a
/// device pairing can ask for one: it gets the narrower of the two.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Everything a device does: the Mac, Linux and iPhone apps.
    #[default]
    Full,
    /// An assistant through `flotilla mcp` (Claude Code, OpenClaw, Hermes): it reads and
    /// searches threads, sees projects and models, starts threads, sends, answers its agents'
    /// questions, interrupts, archives and opens previews. Nothing else: no deleting, merging,
    /// settings, keys, projects or devices (but signing itself out), and no uploads. Its
    /// reading a thread doesn't mark it read.
    Agent,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Full => "full",
            Scope::Agent => "agent",
        }
    }

    /// As stored: anything but `agent` is a full device's (the column's default).
    pub fn parse(s: &str) -> Scope {
        if s == "agent" {
            Scope::Agent
        } else {
            Scope::Full
        }
    }

    /// The narrower of two: a code's and what the device asked for, say.
    pub fn narrowest(self, other: Scope) -> Scope {
        if self == Scope::Agent || other == Scope::Agent {
            Scope::Agent
        } else {
            Scope::Full
        }
    }
}

/// `abcd efgh-jklm` → `ABCDEFGHJKLM`, if it's a code at all: case and separators don't matter.
pub fn normalize_code(s: &str) -> Option<String> {
    let code: String = s
        .chars()
        .filter(|c| !matches!(c, '-' | ' ' | '\t'))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (code.len() == CODE_LEN && code.bytes().all(|b| CODE_ALPHABET.contains(&b))).then_some(code)
}

/// A code in three fours, for reading out: `ABCD-EFGH-JKLM`.
pub fn display_code(code: &str) -> String {
    code.as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("-")
}

/// The link for `code` at `server` (a URL [`server_url`] accepts).
pub fn link(scheme: &str, server: &str, code: &str) -> String {
    format!("{scheme}://pair?u={}#{code}", encode(server))
}

/// What a pasted link (or a typed server address) says.
#[derive(Clone, PartialEq, Eq)]
pub struct Target {
    /// `https://host` or `http://host:port`.
    pub server: String,
    pub code: Option<String>,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("server", &self.server)
            .field("code", &self.code.as_ref().map(|_| "…"))
            .finish()
    }
}

/// Read a pairing link, a web pairing link (`https://host/pair#CODE`), or just a server's
/// address.
pub fn read(text: &str) -> Result<Target, String> {
    let text = text.trim();
    let (before, fragment) = match text.split_once('#') {
        Some((b, f)) => (b, Some(f)),
        None => (text, None),
    };
    let code = match fragment.map(str::trim).filter(|f| !f.is_empty()) {
        Some(f) => Some(normalize_code(f).ok_or("The link's code isn't right")?),
        None => None,
    };
    let lower = before.to_ascii_lowercase();
    let server = if let Some(rest) = [SCHEME, DEV_SCHEME].iter().find_map(|s| {
        lower
            .strip_prefix(&format!("{s}://"))
            .map(|_| &before[s.len() + 3..])
    }) {
        let query = rest
            .strip_prefix("pair")
            .map(|q| q.trim_start_matches('/'))
            .and_then(|q| q.strip_prefix('?'))
            .ok_or("That isn't a pairing link")?;
        let u = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("u="))
            .ok_or("The link doesn't say which server")?;
        decode(u).ok_or("The link's server isn't readable")?
    } else {
        // A web pairing link's page.
        before
            .strip_suffix("/pair")
            .or_else(|| before.strip_suffix("/pair/"))
            .unwrap_or(before)
            .to_string()
    };
    Ok(Target {
        server: server_url(&server)?,
        code,
    })
}

/// A server's address as typed, as a URL: `ws(s)://` reads as `http(s)://`, and with no
/// scheme, `host:port` is `http://` and a bare host `https://` (a Tailscale name).
pub fn server_url(text: &str) -> Result<String, String> {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    let (scheme, rest) = if let Some(r) = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("wss://"))
    {
        ("https", &text[text.len() - r.len()..])
    } else if let Some(r) = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("ws://"))
    {
        ("http", &text[text.len() - r.len()..])
    } else if lower.contains("://") {
        return Err(format!(
            "{text} isn't a server's address (http:// or https://)"
        ));
    } else {
        (if has_port(text) { "http" } else { "https" }, text)
    };
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() {
        return Err("Say the server's address".into());
    }
    let colon_ok =
        !rest.contains(':') || has_port(rest) || (rest.starts_with('[') && rest.ends_with(']'));
    if rest.contains(['/', '?', '#', '@', ' ']) || !colon_ok {
        return Err(format!(
            "{text} isn't just a server's address: give its host (and port), like https://flotilla.tail1234.ts.net"
        ));
    }
    Ok(format!("{scheme}://{rest}"))
}

/// `host:port` (and `[v6]:port`), not a bare host or `[v6]`.
fn has_port(authority: &str) -> bool {
    match authority.rsplit_once(':') {
        Some((host, port)) => {
            !port.is_empty()
                && port.bytes().all(|b| b.is_ascii_digit())
                && (!host.contains(':') || host.ends_with(']'))
        }
        None => false,
    }
}

fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3 / 2);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_carry_the_server_in_the_query_and_the_code_in_the_fragment() {
        let l = link(SCHEME, "https://flotilla.tail1234.ts.net", "ABCDEFGHJKMN");
        assert_eq!(
            l,
            "flotilla://pair?u=https%3A%2F%2Fflotilla.tail1234.ts.net#ABCDEFGHJKMN"
        );
        assert_eq!(
            read(&l).unwrap(),
            Target {
                server: "https://flotilla.tail1234.ts.net".into(),
                code: Some("ABCDEFGHJKMN".into())
            }
        );
        let dev = link(DEV_SCHEME, "http://127.0.0.1:7777", "ABCDEFGHJKMN");
        assert_eq!(read(&dev).unwrap().server, "http://127.0.0.1:7777");
        // Typed by hand: any case, grouped, spaces around.
        assert_eq!(
            read("  flotilla://pair?u=http%3a%2f%2f127.0.0.1%3a7777#abcd-efgh-jkmn ")
                .unwrap()
                .code
                .as_deref(),
            Some("ABCDEFGHJKMN")
        );
        // A web link, a bare address.
        assert_eq!(
            read("https://box.tail1.ts.net/pair#ABCDEFGHJKMN").unwrap(),
            Target {
                server: "https://box.tail1.ts.net".into(),
                code: Some("ABCDEFGHJKMN".into())
            }
        );
        assert_eq!(
            read("box.tail1.ts.net").unwrap(),
            Target {
                server: "https://box.tail1.ts.net".into(),
                code: None
            }
        );
        for bad in [
            "flotilla://pair#ABCDEFGHJKMN",
            "flotilla://other?u=x",
            "flotilla://pair?u=https%3A%2F%2Fx#WRONG",
            "ftp://x",
            "https://x/some/path",
            "",
        ] {
            assert!(read(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn server_addresses_read_as_urls() {
        for (typed, url) in [
            ("https://a.ts.net/", "https://a.ts.net"),
            ("wss://a.ts.net", "https://a.ts.net"),
            ("ws://127.0.0.1:7777", "http://127.0.0.1:7777"),
            ("127.0.0.1:7777", "http://127.0.0.1:7777"),
            ("[::1]:7777", "http://[::1]:7777"),
            ("a.ts.net", "https://a.ts.net"),
            ("HTTP://Box:80", "http://Box:80"),
        ] {
            assert_eq!(server_url(typed).unwrap(), url, "{typed}");
        }
        for bad in [
            "",
            "https://",
            "https:",
            "https://user@host",
            "https://host/path",
            "https://host?x",
            "file:///etc",
            "host:port",
        ] {
            assert!(server_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_scope_is_the_narrower_of_two_and_reads_as_stored() {
        assert_eq!(Scope::Full.narrowest(Scope::Agent), Scope::Agent);
        assert_eq!(Scope::Agent.narrowest(Scope::Full), Scope::Agent);
        assert_eq!(Scope::Full.narrowest(Scope::Full), Scope::Full);
        assert_eq!(Scope::parse("agent"), Scope::Agent);
        assert_eq!(Scope::parse("full"), Scope::Full);
        assert_eq!(Scope::parse(""), Scope::Full);
        assert_eq!(serde_json::to_string(&Scope::Agent).unwrap(), "\"agent\"");
    }

    #[test]
    fn codes_read_however_typed() {
        assert_eq!(
            normalize_code(" abcd efgh-jkmn "),
            Some("ABCDEFGHJKMN".into())
        );
        assert_eq!(display_code("ABCDEFGHJKMN"), "ABCD-EFGH-JKMN");
        for bad in [
            "",
            "ABCDEFGHJKM",
            "ABCDEFGHJKMNP",
            "ABCDEFGHJKM0",
            "ABCDEFGHJKMI",
        ] {
            assert_eq!(normalize_code(bad), None, "{bad}");
        }
    }
}
