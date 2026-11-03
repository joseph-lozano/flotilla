//! What Flotilla keeps in every thread's VM: the `flotilla` command agents hand work on with,
//! and the thread's key it signs its requests with (`~/.flotilla/key`, only the VM's user may
//! read it; see `crate::orchestrate`);
//! a section in the instructions file each harness reads at home (Claude Code's
//! `~/.claude/CLAUDE.md`, Codex's `~/.codex/AGENTS.md`, OpenCode's
//! `~/.config/opencode/AGENTS.md`), with your instructions from Settings first, between
//! markers so the rest of each file is left alone; and your skills, in `~/.claude/skills`
//! (Claude Code and OpenCode read it) and `~/.codex/skills`, from the packed copies clients
//! uploaded. Written before a prompt whenever what's wanted differs from what the VM last got.

use crate::boat::BoatClient;
use crate::error::{Error, Result};
use crate::setup::sh_quote;
use crate::store::Store;
use base64::Engine;
use std::hash::{DefaultHasher, Hash, Hasher};

/// The instructions files, relative to the home folder.
pub const INSTRUCTION_FILES: [&str; 3] = [
    ".claude/CLAUDE.md",
    ".codex/AGENTS.md",
    ".config/opencode/AGENTS.md",
];

const BEGIN: &str = "<!-- flotilla:begin -->";
const END: &str = "<!-- flotilla:end -->";

/// Bumped when the way files are written changes, so every VM gets them again.
const VERSION: u32 = 4;

/// Where the packed skills wait in the VM, one `<n>.tgz.b64` each.
const INBOX: &str = "$HOME/.flotilla/skills.in";

/// The packed skills go up in pieces this big, each its own request: a command is one
/// argument to a shell in the VM, and Linux allows one at most 128 KiB.
const CHUNK: usize = 64 * 1024;

fn b64(s: &str) -> String {
    b64_bytes(s.as_bytes())
}

fn b64_bytes(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// The section of instructions every agent gets: yours, then Flotilla's (handing work on, and
/// asking you).
pub fn instructions(store: &Store) -> String {
    let yours = crate::skills::instructions(store);
    let ours = format!(
        "{}\n\n{}",
        crate::orchestrate::INSTRUCTIONS.trim_end(),
        crate::orchestrate::ASK_INSTRUCTIONS.trim_end()
    );
    if yours.trim().is_empty() {
        ours
    } else {
        format!("{}\n\n{ours}", yours.trim())
    }
}

/// The bash that puts it all in place, with the thread's `key`. With `skills`, the packed
/// skills are waiting in `~/.flotilla/skills.in`; either way the skills it put there before are
/// replaced (a folder it didn't put there, like boat's own skill, is left alone). Nothing it
/// runs prints the key.
pub fn command(store: &Store, key: &str, skills: bool) -> String {
    let section = instructions(store);
    let files = INSTRUCTION_FILES
        .iter()
        .map(|f| format!("\"$HOME/{f}\""))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r#"set -e
mkdir -p "$HOME/.flotilla/bin" "$HOME/.local/bin"
chmod 700 "$HOME/.flotilla"
(umask 077 && printf '%s' {key} > "$HOME/{key_file}.new" && chmod 600 "$HOME/{key_file}.new" && mv -f "$HOME/{key_file}.new" "$HOME/{key_file}")
printf '%s' {script} | base64 -d > "$HOME/.flotilla/bin/flotilla"
chmod +x "$HOME/.flotilla/bin/flotilla"
ln -sf "$HOME/.flotilla/bin/flotilla" "$HOME/.local/bin/flotilla"
sudo -n ln -sf "$HOME/.flotilla/bin/flotilla" /usr/local/bin/flotilla 2>/dev/null || true
section=$(printf '%s' {section} | base64 -d)
for f in {files}; do
  mkdir -p "$(dirname "$f")"
  rest=""
  [ -f "$f" ] && rest=$(sed '/^{begin}$/,/^{end}$/d' "$f")
  {{ [ -n "$rest" ] && printf '%s\n\n' "$rest"; printf '%s\n%s\n%s\n' '{begin}' "$section" '{end}'; }} > "$f.flotilla"
  mv "$f.flotilla" "$f"
done
old=$(cat "$HOME/.flotilla/skills.txt" 2>/dev/null || true)
while IFS= read -r n; do
  [ -n "$n" ] && rm -rf "$HOME/.claude/skills/$n" "$HOME/.codex/skills/$n"
done <<< "$old"
: > "$HOME/.flotilla/skills.txt"
if [ "{skills}" = yes ] && [ -d "{inbox}" ]; then
  new="$HOME/.flotilla/skills.new"
  rm -rf "$new" && mkdir -p "$new" "$HOME/.claude/skills" "$HOME/.codex/skills"
  for f in "{inbox}"/*.tgz.b64; do
    [ -f "$f" ] && base64 -d < "$f" | tar -xzf - -C "$new"
  done
  for d in "$new"/*/; do
    [ -d "$d" ] || continue
    n=$(basename "$d")
    [ -e "$HOME/.claude/skills/$n" ] || [ -e "$HOME/.codex/skills/$n" ] && continue
    # Listed first: a copy that fails halfway is still ours to clear next time.
    printf '%s\n' "$n" >> "$HOME/.flotilla/skills.txt"
    cp -R "$d" "$HOME/.claude/skills/$n"
    cp -R "$d" "$HOME/.codex/skills/$n"
  done
  rm -rf "$new"
fi
rm -rf "{inbox}" "$HOME/.flotilla/skills.tgz.b64"
"#,
        script = sh_quote(&b64(crate::orchestrate::SCRIPT)),
        key = sh_quote(key),
        key_file = crate::orchestrate::KEY_FILE,
        section = sh_quote(&b64(&section)),
        begin = BEGIN,
        end = END,
        inbox = INBOX,
        skills = if skills { "yes" } else { "no" },
    )
}

/// What the thread's VM should have, in short: the files, the thread's key and the skills as
/// they are now (a skill's blob is its packed folder's hash), in that VM (a thread given a new
/// one gets them all again).
fn wanted(store: &Store, thread_id: &str) -> String {
    let mut h = DefaultHasher::new();
    let sandbox = store
        .thread(thread_id)
        .ok()
        .flatten()
        .and_then(|t| t.sandbox_id);
    (
        VERSION,
        crate::orchestrate::SCRIPT,
        instructions(store),
        crate::orchestrate::key(store, thread_id),
        sandbox,
    )
        .hash(&mut h);
    for s in crate::skills::chosen(store) {
        (s.name, s.blob).hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

/// The skills' packed folders, base64'd, each to go in a file of its own; and why any are
/// missing (too big together, or gone).
fn packed(store: &Store) -> (Vec<String>, Option<String>) {
    let blobs = crate::blobs::Blobs::of(store);
    let (mut out, mut total, mut why) = (Vec::new(), 0usize, None);
    for s in crate::skills::chosen(store) {
        match blobs.read(&s.blob) {
            Ok(bytes) if total + bytes.len() > crate::skills::MAX_PACKED => {
                why.get_or_insert_with(|| {
                    format!(
                        "the skills come to over {} MB packed: {} was left out",
                        crate::skills::MAX_PACKED / (1024 * 1024),
                        s.name
                    )
                });
            }
            Ok(bytes) => {
                total += bytes.len();
                out.push(b64_bytes(&bytes));
            }
            Err(_) => {
                why.get_or_insert_with(|| {
                    format!("{}'s files are gone from the server: add it again", s.name)
                });
            }
        }
    }
    (out, why)
}

fn key(thread_id: &str) -> String {
    format!("provisioned:{thread_id}")
}

/// Whether the thread's VM lacks what it should have now.
pub fn needed(store: &Store, thread_id: &str) -> bool {
    store.kv_get(&key(thread_id)).ok().flatten().as_deref()
        != Some(wanted(store, thread_id).as_str())
}

/// Write it all into the thread's VM, which must be up.
pub fn write(boat: &BoatClient, store: &Store, thread_id: &str, sandbox: &str) -> Result<()> {
    let thread_key = crate::orchestrate::key_or_new(store, thread_id)?;
    let want = wanted(store, thread_id);
    // Skills that can't go (too big, gone) go without: Settings says why.
    let (packed, why) = packed(store);
    crate::skills::set_error(store, why.as_deref())?;
    for (n, skill) in packed.iter().enumerate() {
        for (i, chunk) in skill.as_bytes().chunks(CHUNK).enumerate() {
            let to = if i == 0 { ">" } else { ">>" };
            let chunk = std::str::from_utf8(chunk).unwrap_or_default();
            // The very first piece clears what an earlier write may have left.
            let clear = if n == 0 && i == 0 {
                format!("rm -rf \"{INBOX}\" && ")
            } else {
                String::new()
            };
            let put = format!(
                "{clear}mkdir -p \"{INBOX}\" && printf '%s' {} {to} \"{INBOX}/{n}.tgz.b64\"",
                sh_quote(chunk)
            );
            let res = boat.exec(sandbox, &format!("bash -c {}", sh_quote(&put)), None, 30)?;
            if !res.ok() {
                return Err(Error::Invalid(format!(
                    "couldn't copy the skills: {}",
                    crate::util::ellipsize(res.stderr.trim(), 200)
                )));
            }
        }
    }
    let res = boat.exec(
        sandbox,
        &format!(
            "bash -c {}",
            sh_quote(&command(store, &thread_key, !packed.is_empty()))
        ),
        None,
        30,
    )?;
    if !res.ok() {
        return Err(Error::Invalid(format!(
            "couldn't set up the agent's files: {}",
            crate::util::ellipsize(res.stderr.trim(), 200)
        )));
    }
    store.kv_set(&key(thread_id), &want)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// The command run in a scratch home: the script lands, each instructions file gets the
    /// section once, and what else a file says stays. Skills go the whole way: packed on a
    /// device, uploaded, kept as blobs, copied in and unpacked.
    #[test]
    fn writes_files_and_keeps_the_rest() {
        let home =
            std::env::temp_dir().join(format!("flotilla-provision-{}", uuid::Uuid::new_v4()));
        let store = Store::open(&home.join("state/flotilla.db")).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/CLAUDE.md"), "# Mine\nKeep this.\n").unwrap();
        // Two skills of yours, and boat's own, which stays as it is.
        let mac = home.join("mac-skills");
        for name in ["review", "boat"] {
            std::fs::create_dir_all(mac.join(name)).unwrap();
            std::fs::write(
                mac.join(name).join("SKILL.md"),
                format!("---\ndescription: my {name}\n---\n"),
            )
            .unwrap();
        }
        std::fs::create_dir_all(home.join(".claude/skills/boat")).unwrap();
        std::fs::write(home.join(".claude/skills/boat/SKILL.md"), "boat's own").unwrap();
        let blobs = crate::blobs::Blobs::of(&store);
        for name in ["review", "boat"] {
            let skill = crate::skills::read(&mac.join(name)).unwrap();
            let blob = blobs
                .put(&crate::skills::pack(std::slice::from_ref(&skill)).unwrap())
                .unwrap();
            crate::skills::put(
                &store,
                crate::skills::UserSkill {
                    blob: blob.id,
                    size: blob.size,
                    ..crate::skills::tests_skill(name, "")
                },
            )
            .unwrap();
        }
        crate::skills::set_instructions(&store, "Use pnpm, not npm.").unwrap();
        let thread_key = crate::orchestrate::key_or_new(&store, "t1").unwrap();
        let run = || {
            let (packed, why) = packed(&store);
            assert_eq!((packed.len(), why), (2, None));
            let inbox = home.join(".flotilla/skills.in");
            std::fs::create_dir_all(&inbox).unwrap();
            for (n, p) in packed.iter().enumerate() {
                std::fs::write(inbox.join(format!("{n}.tgz.b64")), p).unwrap();
            }
            let out = Command::new("bash")
                .arg("-c")
                .arg(command(&store, &thread_key, true))
                .env("HOME", &home)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(!inbox.exists());
            // Nothing it runs says the key.
            assert!(
                !String::from_utf8_lossy(&out.stdout).contains(&thread_key)
                    && !String::from_utf8_lossy(&out.stderr).contains(&thread_key)
            );
        };
        run();
        run();
        assert_eq!(
            std::fs::read_to_string(home.join(".claude/skills/review/SKILL.md")).unwrap(),
            "---\ndescription: my review\n---\n"
        );
        assert!(home.join(".codex/skills/review/SKILL.md").exists());
        assert_eq!(
            std::fs::read_to_string(home.join(".claude/skills/boat/SKILL.md")).unwrap(),
            "boat's own"
        );
        assert_eq!(
            std::fs::read_to_string(home.join(".flotilla/skills.txt")).unwrap(),
            "review\n"
        );
        // Taken off in Settings: gone from the VM too.
        crate::skills::remove(&store, "review").unwrap();
        crate::skills::remove(&store, "boat").unwrap();
        let out = Command::new("bash")
            .arg("-c")
            .arg(command(&store, &thread_key, false))
            .env("HOME", &home)
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(
            !home.join(".claude/skills/review").exists()
                && !home.join(".codex/skills/review").exists()
        );
        assert!(home.join(".claude/skills/boat").exists());
        let claude = std::fs::read_to_string(home.join(".claude/CLAUDE.md")).unwrap();
        assert!(claude.starts_with("# Mine\nKeep this.\n\n<!-- flotilla:begin -->\nUse pnpm, not npm.\n\n## Getting help from other agents"), "{claude}");
        assert_eq!(claude.matches(BEGIN).count(), 1);
        assert!(claude.ends_with("<!-- flotilla:end -->\n"));
        let codex = std::fs::read_to_string(home.join(".codex/AGENTS.md")).unwrap();
        assert!(codex.starts_with("<!-- flotilla:begin -->\n"));
        // Every harness is told to ask you with `flotilla ask`, once, after handing work on.
        let opencode = std::fs::read_to_string(home.join(".config/opencode/AGENTS.md")).unwrap();
        for told in [&claude, &codex, &opencode] {
            assert_eq!(told.matches("## Asking the user").count(), 1, "{told}");
            assert!(
                told.find("## Getting help from other agents") < told.find("## Asking the user"),
                "{told}"
            );
            assert!(
                told.contains("flotilla ask \"Which database should I use?\"")
                    && told.ends_with("final message instead.\n<!-- flotilla:end -->\n"),
                "{told}"
            );
        }
        let script = std::fs::read_to_string(home.join(".flotilla/bin/flotilla")).unwrap();
        assert_eq!(script, crate::orchestrate::SCRIPT);
        // The thread's key, which only the VM's user may read.
        use std::os::unix::fs::PermissionsExt;
        let key_file = home.join(crate::orchestrate::KEY_FILE);
        assert_eq!(std::fs::read_to_string(&key_file).unwrap(), thread_key);
        assert_eq!(
            std::fs::metadata(&key_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(home.join(".flotilla"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(needed(&store, "t1"));
        store.kv_set(&key("t1"), &wanted(&store, "t1")).unwrap();
        assert!(!needed(&store, "t1"));
        // A thread whose key is new (its database rebuilt, or the thread forgotten and found
        // again) gets it written again.
        store.kv_delete("orch:key:t1").unwrap();
        crate::orchestrate::key_or_new(&store, "t1").unwrap();
        assert!(needed(&store, "t1"));
        store.kv_set(&key("t1"), &wanted(&store, "t1")).unwrap();
        // A skill whose files are gone goes without, and Settings says why.
        crate::skills::put(
            &store,
            crate::skills::tests_skill("lost", &format!("b_{}", "0".repeat(64))),
        )
        .unwrap();
        let (packed, why) = packed(&store);
        assert!(packed.is_empty() && why.unwrap().contains("lost"));
        assert!(needed(&store, "t1"));
        std::fs::remove_dir_all(&home).unwrap();
    }
}
