//! The devices paired with a server, and the one-time codes that pair them (plan §4).
//!
//! - **Codes** are 12 characters from an alphabet with nothing to misread (no 0/O, 1/I), about
//!   60 bits, valid for 5 minutes by default and used once. Only their hash is kept. A code is
//!   spent by one `UPDATE … RETURNING` that also checks it's unused, unrevoked and unexpired,
//!   in the same transaction as the device it makes, so two tries with one code can't both
//!   pair, and a failed pairing doesn't spend it.
//! - **Device tokens** are `fdt_<device id>_<secret>`: the id says which row to read, and the
//!   whole token's SHA-256 is compared with the row's in constant time. 256 random bits, so
//!   the hash can't be worked back. A device unseen for 90 days lapses (each connection moves
//!   that on); revoking one deletes its row.
//!
//! The server checks tokens; `flotilla pair` and `flotilla devices` write here directly, so they
//! work even when the server doesn't answer.

use crate::error::{Error, Result};
use crate::store::Store;
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::time::Duration;

pub use flotilla_proto::Scope;
pub use flotilla_proto::pair::{CODE_ALPHABET, CODE_LEN, display_code, normalize_code};

/// How long a code works, unless asked otherwise, and the longest it may.
pub const CODE_TTL: Duration = Duration::from_secs(5 * 60);
pub const CODE_TTL_MAX: Duration = Duration::from_secs(24 * 60 * 60);
/// Unused codes at once: a paired device can't fill the table.
const OPEN_CODES_MAX: usize = 20;
/// A device not seen for this long lapses.
pub const LAPSE_AFTER: Duration = Duration::from_secs(90 * 24 * 60 * 60);
pub use flotilla_model::devices::{TOKEN_PREFIX, token_device};
/// The longest device name and platform kept.
const NAME_MAX: usize = 64;

/// A code just made: the only time the code itself is seen.
#[derive(Clone, PartialEq, Eq)]
pub struct Code {
    pub id: String,
    /// The 12 characters, as typed (see [`display_code`] for showing them).
    pub code: String,
    pub expires_ms: i64,
}

impl std::fmt::Debug for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Code")
            .field("id", &self.id)
            .field("expires_ms", &self.expires_ms)
            .finish_non_exhaustive()
    }
}

/// A code never used or revoked, for `flotilla pair --list` (never the code).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCode {
    pub id: String,
    pub made_by: String,
    pub created_ms: i64,
    pub expires_ms: i64,
    /// What the device it pairs may do, at most.
    pub scope: Scope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub created_ms: i64,
    pub last_seen_ms: i64,
    pub last_ip: Option<String>,
    pub scope: Scope,
}

/// A device just paired, and its token: the only time the token is seen.
#[derive(Clone, PartialEq, Eq)]
pub struct Paired {
    pub device: String,
    pub token: String,
    pub scope: Scope,
}

impl std::fmt::Debug for Paired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Paired")
            .field("device", &self.device)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

/// Why a code didn't pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairError {
    /// No such code (or not one at all). Counts toward the server's limit on wrong codes.
    Wrong,
    Used,
    Expired,
    Revoked,
    /// The device's name or platform won't do.
    Invalid(String),
    Db(String),
}

impl PairError {
    /// A try that might be guessing: the server counts these.
    pub fn guessing(&self) -> bool {
        matches!(self, PairError::Wrong)
    }
}

impl std::fmt::Display for PairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PairError::Wrong => f.write_str("That pairing code isn't right"),
            PairError::Used => f.write_str("That pairing code was already used. Make a new one: Add device in Settings on a paired device, or `flotilla pair` on the server"),
            PairError::Expired => f.write_str("That pairing code has expired. Make a new one: Add device in Settings on a paired device, or `flotilla pair` on the server"),
            PairError::Revoked => f.write_str("That pairing code was revoked. Make a new one: Add device in Settings on a paired device, or `flotilla pair` on the server"),
            PairError::Invalid(why) => f.write_str(why),
            PairError::Db(e) => write!(f, "The server couldn't pair: {e}"),
        }
    }
}

/// Why a token wasn't taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// Not a token, no such device (revoked), or the wrong secret.
    Unknown,
    /// Not seen for 90 days: it's gone now.
    Lapsed,
    Db(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Unknown => f.write_str("This device isn't paired with this server"),
            AuthError::Lapsed => f.write_str(
                "This device hadn't connected for 90 days, so the server forgot it: pair it again",
            ),
            AuthError::Db(e) => write!(f, "The server couldn't check this device: {e}"),
        }
    }
}

/// Held while a device is revoked from outside the server (`flotilla devices revoke`), and
/// while a rebuild reads the devices, moves the database aside and carries them into the new
/// one ([`Store::backup`]), so a revoke can't land in between and be undone. The OS's lock
/// (`flock`) on a file beside the database: it goes when the holder does.
/// Where a device remote MCP's OAuth made for a code not swapped yet is noted (kv
/// `<this><device id>`, when it was made): carried by a rebuild with the devices, so it's still
/// revoked if its sign-in isn't finished.
pub const SIGN_IN_PENDING: &str = "mcp:pending:";

pub fn hold(db: &std::path::Path) -> Result<std::fs::File> {
    let mut name = db
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "flotilla.db".into());
    name.push(".devices.lock");
    if let Some(dir) = db.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(db.with_file_name(name))?;
    file.lock()?;
    Ok(file)
}

fn sha256_hex(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(|e| Error::Invalid(format!("no randomness: {e}")))?;
    Ok(buf)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Equal, taking as long whatever the bytes.
fn same(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// Trimmed, without control characters, and not too long.
fn clean(s: &str, what: &str) -> std::result::Result<String, PairError> {
    let s: String = s
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string();
    if s.is_empty() {
        return Err(PairError::Invalid(format!("Say the device's {what}")));
    }
    Ok(crate::util::truncate_chars(&s, NAME_MAX))
}

fn lapse_ms() -> i64 {
    LAPSE_AFTER.as_millis() as i64
}

fn db(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Store {
    /// Make a pairing code that works for `ttl` (at most a day). `made_by`: `cli`, `server` or
    /// the device that asked.
    pub fn mint_code(&self, ttl: Duration, made_by: &str, now_ms: i64) -> Result<Code> {
        self.mint_code_as(ttl, made_by, Scope::Full, now_ms)
    }

    /// [`mint_code`](Store::mint_code) for a device that may do no more than `scope`
    /// (`flotilla pair --agent`).
    pub fn mint_code_as(
        &self,
        ttl: Duration,
        made_by: &str,
        scope: Scope,
        now_ms: i64,
    ) -> Result<Code> {
        let ttl = ttl.min(CODE_TTL_MAX).max(Duration::from_secs(1));
        let tx = self.write()?;
        // Spent and lapsed codes are kept a day, for `--list`'s sake; then they go.
        let day = 24 * 60 * 60 * 1000;
        tx.execute(
            "DELETE FROM pairing_codes WHERE expires_ms < ?1",
            [now_ms - day],
        )?;
        let open: i64 = tx.query_row(
            "SELECT COUNT(*) FROM pairing_codes WHERE consumed_ms IS NULL AND revoked_ms IS NULL AND expires_ms > ?1",
            [now_ms],
            |r| r.get(0),
        )?;
        if open as usize >= OPEN_CODES_MAX {
            return Err(Error::Invalid(format!(
                "There are already {open} unused pairing codes: revoke some with `flotilla pair --revoke`"
            )));
        }
        let expires_ms = now_ms + ttl.as_millis() as i64;
        for _ in 0..4 {
            let bytes = random::<CODE_LEN>()?;
            // 256 is a multiple of 32, so each character is uniform.
            let code: String = bytes
                .iter()
                .map(|b| CODE_ALPHABET[(*b % 32) as usize] as char)
                .collect();
            let id = format!("c_{}", hex(&random::<4>()?));
            let made = tx.execute(
                "INSERT OR IGNORE INTO pairing_codes (id, code_hash, made_by, created_ms, expires_ms, scope) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, sha256_hex(&code), made_by, now_ms, expires_ms, scope.as_str()],
            )?;
            if made == 1 {
                tx.commit()?;
                return Ok(Code {
                    id,
                    code,
                    expires_ms,
                });
            }
        }
        Err(Error::Invalid("couldn't make a pairing code".into()))
    }

    /// Codes never used or revoked: those that would still pair, and those that expired
    /// unused (pruned when a code is made a day or more after they expired).
    pub fn unused_codes(&self) -> Result<Vec<OpenCode>> {
        let mut st = self.conn().prepare(
            "SELECT id, made_by, created_ms, expires_ms, scope FROM pairing_codes
             WHERE consumed_ms IS NULL AND revoked_ms IS NULL ORDER BY created_ms",
        )?;
        let rows = st.query_map([], |r| {
            Ok(OpenCode {
                id: r.get(0)?,
                made_by: r.get(1)?,
                created_ms: r.get(2)?,
                expires_ms: r.get(3)?,
                scope: Scope::parse(&r.get::<_, String>(4)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Revoke an unused code, by its id or the code itself. False if there was none to revoke.
    pub fn revoke_code(&self, id_or_code: &str, now_ms: i64) -> Result<bool> {
        let hash = normalize_code(id_or_code)
            .map(|c| sha256_hex(&c))
            .unwrap_or_default();
        let n = self.conn().execute(
            "UPDATE pairing_codes SET revoked_ms=?1 WHERE (id=?2 OR code_hash=?3) AND consumed_ms IS NULL AND revoked_ms IS NULL",
            params![now_ms, id_or_code.trim(), hash],
        )?;
        Ok(n > 0)
    }

    /// Spend `code` on a new device. Only one try with a code can win; a try that fails spends
    /// nothing.
    pub fn pair_device(
        &self,
        code: &str,
        name: &str,
        platform: &str,
        ip: Option<&str>,
        now_ms: i64,
    ) -> std::result::Result<Paired, PairError> {
        self.pair_device_as(code, name, platform, Scope::Full, ip, now_ms)
    }

    /// [`pair_device`](Store::pair_device), the device asking for no more than `asked`: it may
    /// do the narrower of that and what the code allows.
    pub fn pair_device_as(
        &self,
        code: &str,
        name: &str,
        platform: &str,
        asked: Scope,
        ip: Option<&str>,
        now_ms: i64,
    ) -> std::result::Result<Paired, PairError> {
        self.pair_device_noting(code, name, platform, asked, ip, now_ms, None)
    }

    /// [`pair_device_as`](Store::pair_device_as), noting the new device under `note` (kv
    /// `<note><device id>`, when it was made) in the same write: one a sign-in made before its
    /// client has the token, so it's found (and revoked) however the sign-in ends, a crash
    /// included.
    #[allow(clippy::too_many_arguments)]
    pub fn pair_device_noting(
        &self,
        code: &str,
        name: &str,
        platform: &str,
        asked: Scope,
        ip: Option<&str>,
        now_ms: i64,
        note: Option<&str>,
    ) -> std::result::Result<Paired, PairError> {
        let name = clean(name, "name")?;
        let platform = clean(platform, "platform")?;
        let Some(code) = normalize_code(code) else {
            return Err(PairError::Wrong);
        };
        let hash = sha256_hex(&code);
        let tx = self.write().map_err(db).map_err(PairError::Db)?;
        let spent: Option<(String, String)> = tx
            .query_row(
                "UPDATE pairing_codes SET consumed_ms=?1
                 WHERE code_hash=?2 AND consumed_ms IS NULL AND revoked_ms IS NULL AND expires_ms > ?1
                 RETURNING id, scope",
                params![now_ms, hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db)
            .map_err(PairError::Db)?;
        let Some((code_id, allowed)) = spent else {
            // Say why, for whoever has the code (a guess gets "isn't right").
            let row: Option<(Option<i64>, Option<i64>, i64)> = tx
                .query_row("SELECT consumed_ms, revoked_ms, expires_ms FROM pairing_codes WHERE code_hash=?1", [&hash], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()
                .map_err(db)
                .map_err(PairError::Db)?;
            return Err(match row {
                None => PairError::Wrong,
                Some((Some(_), _, _)) => PairError::Used,
                Some((_, Some(_), _)) => PairError::Revoked,
                Some(_) => PairError::Expired,
            });
        };
        let (device, token) = new_token().map_err(db).map_err(PairError::Db)?;
        let scope = Scope::parse(&allowed).narrowest(asked);
        tx.execute(
            "INSERT INTO devices (id, token_hash, name, platform, created_ms, last_seen_ms, last_ip, scope) VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7)",
            params![device, sha256_hex(&token), name, platform, now_ms, ip, scope.as_str()],
        )
        .map_err(db)
        .map_err(PairError::Db)?;
        tx.execute(
            "UPDATE pairing_codes SET device_id=?1 WHERE id=?2",
            params![device, code_id],
        )
        .map_err(db)
        .map_err(PairError::Db)?;
        if let Some(note) = note {
            tx.execute(
                "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
                params![format!("{note}{device}"), now_ms.to_string(), crate::util::now_iso()],
            )
            .map_err(db)
            .map_err(PairError::Db)?;
        }
        tx.commit().map_err(db).map_err(PairError::Db)?;
        Ok(Paired {
            device,
            token,
            scope,
        })
    }

    /// The sign-in that made device `id` (noted under `note`, as [`pair_device_noting`]
    /// noted it) finished: the note goes, in one write with checking the device is still
    /// paired, so a revoke that got there first wins. Whether it is.
    ///
    /// [`pair_device_noting`]: Store::pair_device_noting
    pub fn settle_noted(&self, note: &str, id: &str) -> Result<bool> {
        let tx = self.write()?;
        let paired = tx
            .query_row("SELECT 1 FROM devices WHERE id=?1", [id], |_| Ok(()))
            .optional()?
            .is_some();
        tx.execute("DELETE FROM kv WHERE key=?1", [format!("{note}{id}")])?;
        tx.commit()?;
        Ok(paired)
    }

    /// Revoke device `id` if it's still noted under `note` (a sign-in that didn't finish), and
    /// only if noted at or before `before_ms` when that's given, in one write with forgetting
    /// the note: a sign-in that finished meanwhile keeps its device. Whether it was revoked.
    pub fn revoke_noted(&self, note: &str, id: &str, before_ms: Option<i64>) -> Result<bool> {
        let key = format!("{note}{id}");
        let tx = self.write()?;
        let at: Option<String> = tx
            .query_row(
                "SELECT value FROM kv WHERE key=?1 AND value != ''",
                [&key],
                |r| r.get(0),
            )
            .optional()?;
        if !at.is_some_and(|v| before_ms.is_none_or(|b| v.parse::<i64>().is_ok_and(|at| at <= b))) {
            return Ok(false);
        }
        tx.execute("DELETE FROM devices WHERE id=?1", [id])?;
        tx.execute("DELETE FROM kv WHERE key=?1", [&key])?;
        tx.commit()?;
        Ok(true)
    }

    /// The device a token is, if it's still paired: seen now, from `ip`.
    pub fn authenticate(
        &self,
        token: &str,
        ip: Option<&str>,
        now_ms: i64,
    ) -> std::result::Result<Device, AuthError> {
        let Some(id) = token_device(token) else {
            return Err(AuthError::Unknown);
        };
        let row: Option<(String, Device)> = self
            .conn()
            .query_row("SELECT token_hash, id, name, platform, created_ms, last_seen_ms, last_ip, scope FROM devices WHERE id=?1", [id], |r| Ok((r.get(0)?, device_row(r, 1)?)))
            .optional()
            .map_err(|e| AuthError::Db(e.to_string()))?;
        let Some((hash, mut device)) = row else {
            return Err(AuthError::Unknown);
        };
        if !same(&sha256_hex(token), &hash) {
            return Err(AuthError::Unknown);
        }
        if device.last_seen_ms + lapse_ms() < now_ms {
            let _ = self.conn().execute("DELETE FROM devices WHERE id=?1", [id]);
            return Err(AuthError::Lapsed);
        }
        self.conn()
            .execute(
                "UPDATE devices SET last_seen_ms=?1, last_ip=COALESCE(?2, last_ip) WHERE id=?3",
                params![now_ms, ip, id],
            )
            .map_err(|e| AuthError::Db(e.to_string()))?;
        device.last_seen_ms = now_ms;
        if ip.is_some() {
            device.last_ip = ip.map(str::to_string);
        }
        Ok(device)
    }

    /// Every paired device, most recently seen first. Lapsed ones are forgotten here.
    pub fn devices(&self, now_ms: i64) -> Result<Vec<Device>> {
        self.conn().execute(
            "DELETE FROM devices WHERE last_seen_ms < ?1",
            [now_ms - lapse_ms()],
        )?;
        let mut st = self.conn().prepare("SELECT id, name, platform, created_ms, last_seen_ms, last_ip, scope FROM devices ORDER BY last_seen_ms DESC, id")?;
        let rows = st.query_map([], |r| device_row(r, 0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Each device's id and when it was last seen, cheaply, to notice changes made elsewhere.
    pub fn device_marks(&self) -> Result<Vec<(String, i64)>> {
        let mut st = self
            .conn()
            .prepare("SELECT id, last_seen_ms FROM devices ORDER BY id")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Whether `id` is (still) paired.
    pub fn device_paired(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn()
            .query_row("SELECT 1 FROM devices WHERE id=?1", [id], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// Seen now (it's connected).
    pub fn touch_device(&self, id: &str, now_ms: i64) -> Result<()> {
        self.conn().execute(
            "UPDATE devices SET last_seen_ms=MAX(last_seen_ms, ?1) WHERE id=?2",
            params![now_ms, id],
        )?;
        Ok(())
    }

    /// Name a device (cleaned as a pairing's name is). False if there's no such device.
    pub fn rename_device(&self, id: &str, name: &str) -> Result<bool> {
        let name = clean(name, "name").map_err(|e| Error::Invalid(e.to_string()))?;
        Ok(self.conn().execute(
            "UPDATE devices SET name=?1 WHERE id=?2",
            params![name, id.trim()],
        )? > 0)
    }

    /// Forget a device: its token stops working. False if there was no such device.
    pub fn revoke_device(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn()
            .execute("DELETE FROM devices WHERE id=?1", [id.trim()])?
            > 0)
    }
}

/// A device from its columns, from `at` on: id, name, platform, created, last seen, IP, scope.
fn device_row(r: &rusqlite::Row, at: usize) -> rusqlite::Result<Device> {
    Ok(Device {
        id: r.get(at)?,
        name: r.get(at + 1)?,
        platform: r.get(at + 2)?,
        created_ms: r.get(at + 3)?,
        last_seen_ms: r.get(at + 4)?,
        last_ip: r.get(at + 5)?,
        scope: Scope::parse(&r.get::<_, String>(at + 6)?),
    })
}

/// A new device's id and token.
fn new_token() -> Result<(String, String)> {
    use base64::Engine;
    let id = hex(&random::<8>()?);
    let secret = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random::<32>()?);
    let token = format!("{TOKEN_PREFIX}{id}_{secret}");
    debug_assert_eq!(token_device(&token), Some(id.as_str()));
    Ok((id, token))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    /// A sign-in's device is noted in the write that makes it; then either its client gets the
    /// token (the note goes while it's still paired) or it's revoked (only while it's still
    /// noted, and only once its time is up), whichever comes first: never both.
    #[test]
    fn a_sign_ins_device_is_settled_or_revoked_never_both() {
        let s = store();
        let pair = |s: &Store| {
            let c = s.mint_code_as(CODE_TTL, "cli", Scope::Agent, NOW).unwrap();
            s.pair_device_noting(
                &c.code,
                "Grok",
                "mcp",
                Scope::Agent,
                None,
                NOW,
                Some("note:"),
            )
            .unwrap()
            .device
        };
        let noted = |s: &Store, d: &str| s.kv_get(&format!("note:{d}")).unwrap();
        // Settled first: a revoke after leaves it.
        let first = pair(&s);
        assert_eq!(noted(&s, &first).as_deref(), Some(NOW.to_string().as_str()));
        assert!(s.settle_noted("note:", &first).unwrap());
        assert!(!s.revoke_noted("note:", &first, None).unwrap());
        assert!(s.device_paired(&first).unwrap() && noted(&s, &first).is_none());
        // Revoked first (not before its time): settling after gives no token.
        let second = pair(&s);
        assert!(
            !s.revoke_noted("note:", &second, Some(NOW - 1)).unwrap(),
            "noted after the cutoff"
        );
        assert!(s.revoke_noted("note:", &second, Some(NOW)).unwrap());
        assert!(!s.settle_noted("note:", &second).unwrap());
        assert!(!s.device_paired(&second).unwrap() && noted(&s, &second).is_none());
        // A pairing that fails notes nothing; one made without a note isn't revoked as noted.
        assert!(
            s.pair_device_noting(
                "ABCD-EFGH-JKMN",
                "Grok",
                "mcp",
                Scope::Agent,
                None,
                NOW,
                Some("note:")
            )
            .is_err()
        );
        let c = s.mint_code_as(CODE_TTL, "cli", Scope::Agent, NOW).unwrap();
        let plain = s
            .pair_device_as(&c.code, "Claude Code", "macos", Scope::Agent, None, NOW)
            .unwrap()
            .device;
        assert!(
            !s.revoke_noted("note:", &plain, None).unwrap() && s.device_paired(&plain).unwrap()
        );
        assert_eq!(s.kv_with_prefix("note:").unwrap().len(), 0);
    }

    #[test]
    fn codes_are_twelve_unambiguous_characters_and_read_however_typed() {
        let s = store();
        let c = s.mint_code(CODE_TTL, "cli", NOW).unwrap();
        assert_eq!(c.code.len(), CODE_LEN);
        assert!(c.code.bytes().all(|b| CODE_ALPHABET.contains(&b)));
        assert_eq!(c.expires_ms, NOW + 300_000);
        let shown = display_code(&c.code);
        assert_eq!(shown.len(), 14);
        assert_eq!(
            normalize_code(&shown.to_lowercase()).as_deref(),
            Some(c.code.as_str())
        );
        assert_eq!(
            normalize_code(" abcd efgh jkmn "),
            Some("ABCDEFGHJKMN".into())
        );
        for bad in [
            "",
            "ABCDEFGHJKM",
            "ABCDEFGHJKMNP",
            "ABCDEFGHJKM0",
            "ABCDEFGHJKMI",
            "ABCDEFGHJKM!",
        ] {
            assert_eq!(normalize_code(bad), None, "{bad}");
        }
        // Only the hash is kept.
        let stored: String = s
            .conn()
            .query_row("SELECT code_hash FROM pairing_codes", [], |r| r.get(0))
            .unwrap();
        assert_ne!(stored, c.code);
        assert_eq!(stored, sha256_hex(&c.code));
        // Its Debug never shows it.
        assert!(!format!("{c:?}").contains(&c.code));
    }

    #[test]
    fn a_code_pairs_once() {
        let s = store();
        let c = s.mint_code(CODE_TTL, "cli", NOW).unwrap();
        let paired = s
            .pair_device(
                &display_code(&c.code),
                "My MacBook",
                "macos",
                Some("100.64.0.2"),
                NOW + 1,
            )
            .unwrap();
        assert_eq!(token_device(&paired.token), Some(paired.device.as_str()));
        assert!(!format!("{paired:?}").contains(&paired.token));
        assert_eq!(
            s.pair_device(&c.code, "Another", "macos", None, NOW + 2),
            Err(PairError::Used)
        );
        let devices = s.devices(NOW + 3).unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(
            (
                devices[0].name.as_str(),
                devices[0].platform.as_str(),
                devices[0].last_ip.as_deref()
            ),
            ("My MacBook", "macos", Some("100.64.0.2"))
        );
        // The token's hash, never the token.
        let stored: String = s
            .conn()
            .query_row("SELECT token_hash FROM devices", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, sha256_hex(&paired.token));
        assert!(s.unused_codes().unwrap().is_empty());
    }

    #[test]
    fn expired_revoked_and_wrong_codes_dont_pair_and_say_why() {
        let s = store();
        let c = s.mint_code(Duration::from_secs(60), "cli", NOW).unwrap();
        assert_eq!(
            s.pair_device(&c.code, "x", "macos", None, NOW + 60_000),
            Err(PairError::Expired)
        );
        let c = s.mint_code(CODE_TTL, "cli", NOW).unwrap();
        // Both are unused: the first expired (`pair --list` says so), the second hasn't.
        let unused = s.unused_codes().unwrap();
        assert_eq!(
            unused
                .iter()
                .map(|c| c.expires_ms > NOW + 60_000)
                .collect::<Vec<_>>(),
            [false, true]
        );
        assert!(s.revoke_code(&c.id, NOW).unwrap());
        assert!(!s.revoke_code(&c.id, NOW).unwrap());
        assert_eq!(
            s.pair_device(&c.code, "x", "macos", None, NOW + 1),
            Err(PairError::Revoked)
        );
        // Revoked by the code itself.
        let c = s.mint_code(CODE_TTL, "cli", NOW).unwrap();
        assert!(s.revoke_code(&display_code(&c.code), NOW).unwrap());
        assert_eq!(
            s.pair_device(&c.code, "x", "macos", None, NOW + 1),
            Err(PairError::Revoked)
        );
        assert_eq!(
            s.pair_device("ABCDEFGHJKMN", "x", "macos", None, NOW),
            Err(PairError::Wrong)
        );
        assert_eq!(
            s.pair_device("nonsense", "x", "macos", None, NOW),
            Err(PairError::Wrong)
        );
        assert!(PairError::Wrong.guessing() && !PairError::Used.guessing());
        // A bad name fails before the code is looked at, and doesn't spend it.
        let c = s.mint_code(CODE_TTL, "cli", NOW).unwrap();
        assert!(matches!(
            s.pair_device(&c.code, " \n ", "macos", None, NOW),
            Err(PairError::Invalid(_))
        ));
        assert!(s.pair_device(&c.code, "ok", "macos", None, NOW).is_ok());
        assert_eq!(s.devices(NOW).unwrap().len(), 1);
    }

    #[test]
    fn ttls_are_capped_and_open_codes_are_limited() {
        let s = store();
        let c = s
            .mint_code(Duration::from_secs(10 * 24 * 3600), "cli", NOW)
            .unwrap();
        assert_eq!(c.expires_ms, NOW + CODE_TTL_MAX.as_millis() as i64);
        for _ in 1..OPEN_CODES_MAX {
            s.mint_code(CODE_TTL, "cli", NOW).unwrap();
        }
        assert!(s.mint_code(CODE_TTL, "cli", NOW).is_err());
        // Spent ones don't count, and a day after expiring they're gone.
        let later = NOW + CODE_TTL_MAX.as_millis() as i64 + 1;
        assert!(s.mint_code(CODE_TTL, "cli", later).is_ok());
        let n: i64 = s
            .conn()
            .query_row("SELECT COUNT(*) FROM pairing_codes", [], |r| r.get(0))
            .unwrap();
        assert!(n <= OPEN_CODES_MAX as i64 + 1);
    }

    #[test]
    fn tokens_are_checked_in_full_and_revoking_ends_them() {
        let s = store();
        let c = s.mint_code(CODE_TTL, "cli", NOW).unwrap();
        let p = s.pair_device(&c.code, "Mac", "macos", None, NOW).unwrap();
        let d = s
            .authenticate(&p.token, Some("127.0.0.1"), NOW + 5)
            .unwrap();
        assert_eq!(
            (d.id.as_str(), d.last_seen_ms, d.last_ip.as_deref()),
            (p.device.as_str(), NOW + 5, Some("127.0.0.1"))
        );
        // One character off, the right id with another secret, nonsense: all unknown.
        let mut wrong = p.token.clone();
        let last = wrong.pop().unwrap();
        wrong.push(if last == 'A' { 'B' } else { 'A' });
        assert_eq!(s.authenticate(&wrong, None, NOW), Err(AuthError::Unknown));
        let (_, other) = new_token().unwrap();
        let forged = format!(
            "{TOKEN_PREFIX}{}_{}",
            p.device,
            other.rsplit_once('_').map(|(_, s)| s).unwrap_or("")
        );
        if token_device(&forged).is_some() {
            assert_eq!(s.authenticate(&forged, None, NOW), Err(AuthError::Unknown));
        }
        for bad in [
            "",
            "Bearer x",
            "fdt_",
            "fdt_0123456789abcdef_short",
            &p.token[..p.token.len() - 1],
        ] {
            assert_eq!(
                s.authenticate(bad, None, NOW),
                Err(AuthError::Unknown),
                "{bad}"
            );
        }
        assert!(s.device_paired(&p.device).unwrap());
        assert!(s.revoke_device(&p.device).unwrap());
        assert!(!s.device_paired(&p.device).unwrap());
        assert_eq!(s.authenticate(&p.token, None, NOW), Err(AuthError::Unknown));
        assert!(!s.revoke_device(&p.device).unwrap());
    }

    #[test]
    fn a_device_can_be_renamed_and_its_name_is_cleaned_as_a_pairings_is() {
        let s = store();
        let c = s.mint_code(CODE_TTL, "cli", NOW).unwrap();
        let p = s.pair_device(&c.code, "iPhone", "ios", None, NOW).unwrap();
        let name = |s: &Store| {
            s.devices(NOW)
                .unwrap()
                .into_iter()
                .find(|d| d.id == p.device)
                .map(|d| d.name)
        };
        assert!(s.rename_device(&p.device, "  Work\u{7}phone \n").unwrap());
        assert_eq!(name(&s).as_deref(), Some("Workphone"));
        assert!(s.rename_device(&p.device, &"x".repeat(100)).unwrap());
        assert_eq!(name(&s).map(|n| n.chars().count()), Some(NAME_MAX));
        // Nothing but spaces: refused, and the name stays.
        assert!(
            matches!(s.rename_device(&p.device, " \t "), Err(Error::Invalid(e)) if e.contains("name"))
        );
        assert_eq!(name(&s).map(|n| n.chars().count()), Some(NAME_MAX));
        assert!(!s.rename_device("0123456789abcdef", "Other").unwrap());
        // The token still works.
        assert!(s.authenticate(&p.token, None, NOW).is_ok());
    }

    #[test]
    fn a_device_unseen_for_ninety_days_lapses_and_each_connection_moves_that_on() {
        let s = store();
        let c = s.mint_code(CODE_TTL, "cli", NOW).unwrap();
        let p = s.pair_device(&c.code, "Phone", "ios", None, NOW).unwrap();
        let day = 24 * 3600 * 1000;
        // Seen at day 80: good until day 170.
        assert!(s.authenticate(&p.token, None, NOW + 80 * day).is_ok());
        assert!(s.authenticate(&p.token, None, NOW + 169 * day).is_ok());
        s.touch_device(&p.device, NOW + 200 * day).unwrap();
        assert_eq!(
            s.authenticate(&p.token, None, NOW + 291 * day),
            Err(AuthError::Lapsed)
        );
        assert!(!s.device_paired(&p.device).unwrap());
        // `devices` forgets lapsed ones too.
        let c = s.mint_code(CODE_TTL, "cli", NOW).unwrap();
        s.pair_device(&c.code, "Old", "linux", None, NOW).unwrap();
        assert!(s.devices(NOW + 91 * day).unwrap().is_empty());
    }

    #[test]
    fn a_device_may_do_the_narrower_of_what_its_code_allows_and_what_it_asked_for() {
        let s = store();
        let pair = |code_scope: Scope, asked: Scope| {
            let c = s.mint_code_as(CODE_TTL, "cli", code_scope, NOW).unwrap();
            let p = s
                .pair_device_as(&c.code, "x", "macos", asked, None, NOW)
                .unwrap();
            let d = s.authenticate(&p.token, None, NOW).unwrap();
            assert_eq!(d.scope, p.scope);
            p.scope
        };
        assert_eq!(pair(Scope::Full, Scope::Full), Scope::Full);
        assert_eq!(pair(Scope::Full, Scope::Agent), Scope::Agent);
        // An agent's code pairs an agent, whatever the device asks for.
        assert_eq!(pair(Scope::Agent, Scope::Full), Scope::Agent);
        assert_eq!(pair(Scope::Agent, Scope::Agent), Scope::Agent);
        // `pair --list` says which codes are an agent's.
        let c = s.mint_code_as(CODE_TTL, "cli", Scope::Agent, NOW).unwrap();
        assert!(
            s.unused_codes()
                .unwrap()
                .iter()
                .any(|o| o.id == c.id && o.scope == Scope::Agent)
        );
        let listed = s.devices(NOW).unwrap();
        assert_eq!(listed.iter().filter(|d| d.scope == Scope::Agent).count(), 3);
    }

    #[test]
    fn device_names_are_cleaned() {
        let s = store();
        let c = s.mint_code(CODE_TTL, "cli", NOW).unwrap();
        let long = "x".repeat(200);
        s.pair_device(&c.code, &format!("  Mac\u{7}\n{long}"), "macos", None, NOW)
            .unwrap();
        let d = &s.devices(NOW).unwrap()[0];
        assert!(d.name.starts_with("Macx") && d.name.chars().count() == NAME_MAX);
    }

    #[test]
    fn a_rebuild_keeps_the_paired_devices_and_the_servers_identity() {
        let dir =
            std::env::temp_dir().join(format!("flotilla-devices-rebuild-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = dir.join("flotilla.db");
        let (token, agent) = {
            let s = Store::open(&db).unwrap();
            s.kv_set("server_id", "srv_kept").unwrap();
            s.kv_set("server_url", "https://box.tail1.ts.net").unwrap();
            let c = s.mint_code(CODE_TTL, "cli", NOW).unwrap();
            let mac = s
                .pair_device(&c.code, "Mac", "macos", None, NOW)
                .unwrap()
                .token;
            let c = s.mint_code_as(CODE_TTL, "cli", Scope::Agent, NOW).unwrap();
            (
                mac,
                s.pair_device(&c.code, "Claude Code", "macos", None, NOW)
                    .unwrap()
                    .token,
            )
        };
        // A revoke from outside the server, under way as the rebuild starts: the rebuild
        // waits for it, so it carries the devices as they are after it.
        let (a, _) = {
            let s = Store::open(&db).unwrap();
            let c = s.mint_code(CODE_TTL, "cli", NOW).unwrap();
            let p = s
                .pair_device(&c.code, "Revoked", "macos", None, NOW)
                .unwrap();
            (p.device, p.token)
        };
        let held = hold(&db).unwrap();
        let rebuild = std::thread::spawn({
            let db = db.clone();
            move || Store::backup(&db).unwrap()
        });
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !rebuild.is_finished(),
            "the rebuild didn't wait for the revoke"
        );
        assert!(Store::open(&db).unwrap().revoke_device(&a).unwrap());
        drop(held);
        let bak = rebuild.join().unwrap();
        assert!(bak.exists());
        assert!(!Store::open(&db).unwrap().device_paired(&a).unwrap());
        let s = Store::open(&db).unwrap();
        assert_eq!(
            s.authenticate(&token, None, NOW + 1).unwrap().scope,
            Scope::Full
        );
        // An agent stays one: a rebuild can't widen what it may do.
        assert_eq!(
            s.authenticate(&agent, None, NOW + 1).unwrap().scope,
            Scope::Agent
        );
        assert_eq!(s.kv_get("server_id").unwrap().as_deref(), Some("srv_kept"));
        assert_eq!(
            s.kv_get("server_url").unwrap().as_deref(),
            Some("https://box.tail1.ts.net")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A sign-in not finished across a rebuild is still revoked once its time is up; one
    /// finished before stays paired and isn't noted.
    #[test]
    fn a_rebuild_keeps_a_sign_in_pending() {
        let dir =
            std::env::temp_dir().join(format!("flotilla-devices-pending-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = dir.join("flotilla.db");
        let (pending, settled) = {
            let s = Store::open(&db).unwrap();
            let mut made = Vec::new();
            for _ in 0..2 {
                let c = s.mint_code_as(CODE_TTL, "cli", Scope::Agent, NOW).unwrap();
                made.push(
                    s.pair_device_noting(
                        &c.code,
                        "Grok",
                        "mcp",
                        Scope::Agent,
                        None,
                        NOW,
                        Some(SIGN_IN_PENDING),
                    )
                    .unwrap()
                    .device,
                );
            }
            assert!(s.settle_noted(SIGN_IN_PENDING, &made[1]).unwrap());
            (made[0].clone(), made[1].clone())
        };
        Store::backup(&db).unwrap();
        let s = Store::open(&db).unwrap();
        assert_eq!(
            s.kv_with_prefix(SIGN_IN_PENDING).unwrap(),
            [(format!("{SIGN_IN_PENDING}{pending}"), NOW.to_string())]
        );
        assert!(
            s.revoke_noted(SIGN_IN_PENDING, &pending, Some(NOW))
                .unwrap()
        );
        assert!(!s.device_paired(&pending).unwrap());
        assert!(
            !s.revoke_noted(SIGN_IN_PENDING, &settled, None).unwrap()
                && s.device_paired(&settled).unwrap()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
