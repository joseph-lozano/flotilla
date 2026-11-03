//! Skills on the device that adds them (folders with a SKILL.md): read, found where harnesses
//! keep them, and packed to upload (`flotilla_core::skills` keeps the ones every agent gets).

use flotilla_proto::Skill;
use std::path::{Path, PathBuf};

/// Packed skills at most, so a stray big folder can't stall every prompt.
pub const MAX_PACKED: usize = 8 * 1024 * 1024;

/// The skill in `folder`, if it has a SKILL.md.
pub fn read(folder: &Path) -> Option<Skill> {
    let text = std::fs::read_to_string(folder.join("SKILL.md")).ok()?;
    let name = folder.file_name()?.to_string_lossy().into_owned();
    Some(Skill {
        name,
        description: description(&text),
        folder: folder.to_path_buf(),
    })
}

/// The `description:` in a SKILL.md's front matter (folded lines joined), else its first line
/// of text.
fn description(text: &str) -> String {
    let mut lines = text.lines();
    let mut out = String::new();
    if lines.next().map(str::trim) == Some("---") {
        let mut folding = false;
        for line in lines.by_ref() {
            if line.trim() == "---" {
                break;
            }
            if folding && line.starts_with([' ', '\t']) {
                out.push_str(if out.is_empty() { "" } else { " " });
                out.push_str(line.trim());
                continue;
            }
            folding = false;
            if let Some(v) = line.strip_prefix("description:") {
                let v = v.trim();
                if matches!(v.trim_end_matches(['-', '+']), ">" | "|") {
                    folding = true;
                } else {
                    out = v.trim_matches(['"', '\'']).to_string();
                }
            }
        }
    } else {
        lines = text.lines();
    }
    if out.is_empty() {
        out = lines
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .unwrap_or_default()
            .to_string();
    }
    out
}

/// The skills in the folders harnesses keep them in on this device, by name (the first of a
/// name).
pub fn on_this_mac() -> Vec<Skill> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    in_folders(&[
        home.join(".claude/skills"),
        home.join(".agents/skills"),
        home.join(".codex/skills"),
    ])
}

fn in_folders(roots: &[PathBuf]) -> Vec<Skill> {
    let mut out: Vec<Skill> = Vec::new();
    for root in roots {
        let Ok(dir) = std::fs::read_dir(root) else {
            continue;
        };
        let mut found: Vec<Skill> = dir
            .flatten()
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .filter_map(|e| read(&e.path()))
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        for s in found {
            if !out.iter().any(|o| o.name == s.name) {
                out.push(s);
            }
        }
    }
    out
}

/// The skills as a gzipped tar, each in a folder of its name.
pub fn pack(skills: &[Skill]) -> Result<Vec<u8>, String> {
    if skills.is_empty() {
        return Ok(Vec::new());
    }
    let mut cmd = std::process::Command::new("tar");
    // No macOS metadata: Linux's tar would warn about it, or unpack it as `._` files.
    cmd.env("COPYFILE_DISABLE", "1").args([
        "-czf",
        "-",
        "--exclude",
        ".git",
        "--exclude",
        ".DS_Store",
        "--exclude",
        "node_modules",
    ]);
    if cfg!(target_os = "macos") {
        cmd.args(["--no-mac-metadata", "--no-xattrs"]);
    }
    for s in skills {
        let parent = s
            .folder
            .parent()
            .ok_or_else(|| format!("{} has no parent folder", s.folder.display()))?;
        let dir = s
            .folder
            .file_name()
            .ok_or_else(|| format!("{} has no name", s.folder.display()))?;
        cmd.arg("-C").arg(parent).arg(dir);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("couldn't pack the skills: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "couldn't pack the skills: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    if out.stdout.len() > MAX_PACKED {
        return Err(format!(
            "the skills come to over {} MB packed",
            MAX_PACKED / (1024 * 1024)
        ));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("flotilla-skills-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn descriptions() {
        assert_eq!(
            description("---\nname: x\ndescription: \"Review a PR\"\n---\nBody"),
            "Review a PR"
        );
        assert_eq!(
            description("---\ndescription: >\n  Use the boat CLI\n  for VMs.\nother: 1\n---\n"),
            "Use the boat CLI for VMs."
        );
        assert_eq!(
            description("# Deploy\n\nRun the deploy script.\n"),
            "Run the deploy script."
        );
    }

    #[test]
    fn finds_and_packs_skills() {
        let root = scratch();
        for (dir, name) in [
            ("claude", "review"),
            ("claude", "deploy"),
            ("agents", "review"),
            ("claude", ".system"),
        ] {
            let f = root.join(dir).join(name);
            std::fs::create_dir_all(f.join("scripts")).unwrap();
            std::fs::write(
                f.join("SKILL.md"),
                format!("---\ndescription: {dir} {name}\n---\n"),
            )
            .unwrap();
            std::fs::write(f.join("scripts/run.sh"), "echo hi\n").unwrap();
        }
        std::fs::create_dir_all(root.join("claude/not-a-skill")).unwrap();
        let found = in_folders(&[root.join("claude"), root.join("agents")]);
        assert_eq!(
            found
                .iter()
                .map(|s| (s.name.as_str(), s.description.as_str()))
                .collect::<Vec<_>>(),
            [("deploy", "claude deploy"), ("review", "claude review")]
        );

        let review = read(&root.join("claude/review")).unwrap();
        let packed = pack(std::slice::from_ref(&review)).unwrap();
        let out = root.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let mut tar = std::process::Command::new("tar")
            .args(["-xzf", "-", "-C"])
            .arg(&out)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(tar.stdin.as_mut().unwrap(), &packed).unwrap();
        assert!(tar.wait().unwrap().success());
        assert_eq!(
            std::fs::read_to_string(out.join("review/scripts/run.sh")).unwrap(),
            "echo hi\n"
        );
        assert!(!out.join("._review").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
