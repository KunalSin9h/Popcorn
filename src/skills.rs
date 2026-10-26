//! Claude skills from your computer, for every corn. One way only: Sync copies the skills you
//! pick from ~/.claude/skills into Popcorn's cache and packs the cache into a read-only disk
//! image. Each corn gets that image as a drive and copies the skills into its own
//! ~/.claude/skills when it starts. Nothing in a corn can write to the image, the cache or your
//! skills, and Popcorn never reads anything back.
use crate::sandbox::home;
use crate::share::refuse_secrets;
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Most one skill may hold.
const MAX_SKILL: u64 = 50 << 20;

pub struct Skill {
    pub name: String,
    /// Files in the skill whose names look like secrets, so you can check them before a sync.
    pub flagged: Vec<String>,
}

fn source() -> PathBuf {
    std::env::home_dir().unwrap_or_default().join(".claude/skills")
}

fn chosen_file() -> PathBuf {
    std::env::home_dir().unwrap_or_default().join(".config/popcorn/skills")
}

/// The image every corn gets. It exists after the first sync, also when no skill is chosen, so
/// corns remove skills you stopped syncing.
pub fn image() -> PathBuf {
    home().join("skills.ext4")
}

/// One plain folder name: letters, digits, `.`, `_` and `-`, not hidden.
fn plain(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

fn looks_secret(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with(".env")
        || [".env", ".pem", ".key", ".p12", ".pfx"].iter().any(|e| n.ends_with(e))
        || ["secret", "token", "credential", "password", "apikey", "api_key", "id_rsa", "id_ed25519"].iter().any(|w| n.contains(w))
}

/// The folder of skill `name` in `root`. A skill that is itself a link (such as one installed
/// by another tool) is followed, because you picked it by name, but the target must hold a
/// SKILL.md and must not be your home folder or a folder with keys.
fn folder(root: &Path, name: &str) -> Option<PathBuf> {
    if !plain(name) {
        return None;
    }
    let p = root.join(name).canonicalize().ok()?;
    (p.is_dir() && p.join("SKILL.md").is_file() && refuse_secrets(&p).is_ok()).then_some(p)
}

/// The regular files in a skill as (path inside the skill, file, size, executable). Links
/// inside a skill are never followed, and devices and the like are skipped.
fn files(dir: &Path) -> Vec<(String, PathBuf, u64, bool)> {
    let mut out = Vec::new();
    let mut stack = vec![(String::new(), dir.to_path_buf())];
    while let Some((rel, d)) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let (Ok(m), Some(n)) = (fs::symlink_metadata(e.path()), e.file_name().to_str().map(String::from)) else { continue };
            let r = if rel.is_empty() { n } else { format!("{rel}/{n}") };
            if m.is_dir() {
                stack.push((r, e.path()));
            } else if m.is_file() {
                out.push((r, e.path(), m.len(), m.permissions().mode() & 0o111 != 0));
            }
        }
    }
    out.sort();
    out
}

/// The skills in ~/.claude/skills that can be synced.
pub fn available() -> Vec<Skill> {
    let root = source();
    let mut out: Vec<Skill> = fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let dir = folder(&root, &name)?;
            let flagged = files(&dir).into_iter().filter(|(r, ..)| r.rsplit('/').next().is_some_and(looks_secret)).map(|(r, ..)| r).collect();
            Some(Skill { name, flagged })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn chosen() -> BTreeSet<String> {
    fs::read_to_string(chosen_file()).unwrap_or_default().lines().filter(|l| plain(l)).map(String::from).collect()
}

pub fn set_chosen(names: &BTreeSet<String>) -> Result<(), String> {
    let f = chosen_file();
    fs::create_dir_all(f.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(f, names.iter().cloned().collect::<Vec<_>>().join("\n")).map_err(|e| e.to_string())
}

/// Copy the chosen skills from your computer and rebuild the image. Returns how many skills the
/// corns get. Running corns get them when they start again.
pub fn sync() -> Result<usize, String> {
    pack(&source(), &chosen(), &home())
}

/// Copy `names` from `root` into `out/skills` (regular files only, no setuid bits) and pack that
/// folder into `out/skills.ext4`. Both are replaced only when everything worked.
pub(crate) fn pack(root: &Path, names: &BTreeSet<String>, out: &Path) -> Result<usize, String> {
    let (tmp, img) = (out.join("skills.new"), out.join("skills.ext4.new"));
    fs::remove_dir_all(&tmp).ok();
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let (mut bytes, mut count, mut synced) = (0u64, 0u64, 0usize);
    for name in names {
        // A skill you removed from your computer is dropped from the corns too.
        let Some(dir) = folder(root, name) else { continue };
        let list = files(&dir);
        if list.iter().map(|f| f.2).sum::<u64>() > MAX_SKILL {
            fs::remove_dir_all(&tmp).ok();
            return Err(format!("The skill {name} is larger than 50 MB. Popcorn did not sync."));
        }
        for (rel, path, size, exec) in list {
            let dest = tmp.join(name).join(&rel);
            fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
            fs::copy(&path, &dest).map_err(|e| format!("{name}/{rel}: {e}"))?;
            fs::set_permissions(&dest, fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 })).map_err(|e| e.to_string())?;
            bytes += size.div_ceil(4096) * 4096 + 4096;
            count += 1;
        }
        synced += 1;
    }
    // Room for every file and folder, plus ext4's own data.
    let size_kb = (bytes + (16 << 20)) / 1024;
    let ok = Command::new("mkfs.ext4")
        .args(["-q", "-F", "-L", "skills", "-N", &(count * 2 + 64).to_string(), "-d"])
        .arg(&tmp)
        .arg(&img)
        .arg(format!("{size_kb}k"))
        .status()
        .map_err(|e| e.to_string())?;
    if !ok.success() {
        fs::remove_file(&img).ok();
        return Err("mkfs.ext4 could not pack the skills.".into());
    }
    // Running corns keep the image they opened; the next start gets this one.
    fs::rename(&img, out.join("skills.ext4")).map_err(|e| e.to_string())?;
    fs::remove_dir_all(out.join("skills")).ok();
    fs::rename(&tmp, out.join("skills")).map_err(|e| e.to_string())?;
    Ok(synced)
}

/// Forget synced skills: the cache and the image. Your skills do not change.
pub fn forget() {
    fs::remove_file(image()).ok();
    fs::remove_dir_all(home().join("skills")).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syncs_only_plain_files_of_chosen_skills() {
        let t = std::env::temp_dir().join(format!("popcorn-skills-{}", std::process::id()));
        fs::remove_dir_all(&t).ok();
        let (src, out, elsewhere) = (t.join("src"), t.join("out"), t.join("elsewhere/hf"));
        fs::create_dir_all(src.join("a/scripts")).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        fs::create_dir_all(src.join("no-skill-md")).unwrap();
        fs::write(src.join("a/SKILL.md"), "# a").unwrap();
        fs::write(src.join("a/scripts/run.sh"), "#!/bin/sh").unwrap();
        fs::set_permissions(src.join("a/scripts/run.sh"), fs::Permissions::from_mode(0o4755)).unwrap();
        fs::write(src.join("a/config.env"), "X=1").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", src.join("a/passwd")).unwrap();
        std::os::unix::fs::symlink("/etc", src.join("a/etc")).unwrap();
        fs::write(elsewhere.join("SKILL.md"), "# hf").unwrap();
        std::os::unix::fs::symlink(&elsewhere, src.join("hf")).unwrap();
        std::os::unix::fs::symlink("/etc", src.join("etc-skill")).unwrap();

        assert!(folder(&src, "a").is_some() && folder(&src, "hf").is_some(), "a skill that is a link is followed");
        assert!(folder(&src, "no-skill-md").is_none() && folder(&src, "etc-skill").is_none(), "only folders with a SKILL.md");
        assert!(folder(&src, "../src/a").is_none() && folder(&src, ".hidden").is_none());
        assert!(looks_secret("config.env") && looks_secret(".env.local") && looks_secret("api_token.txt") && !looks_secret("SKILL.md"));

        let names: BTreeSet<String> = ["a", "hf", "etc-skill", "gone"].map(String::from).into();
        assert_eq!(pack(&src, &names, &out).unwrap(), 2, "only real skills; a missing one is dropped");
        let cache = out.join("skills");
        assert!(cache.join("a/SKILL.md").is_file() && cache.join("hf/SKILL.md").is_file());
        assert!(!cache.join("a/passwd").exists() && !cache.join("a/etc").exists(), "links inside a skill are never followed");
        assert_eq!(fs::metadata(cache.join("a/scripts/run.sh")).unwrap().permissions().mode() & 0o7777, 0o755, "no setuid");
        let ls = Command::new("debugfs").args(["-R", "ls -p /a/scripts"]).arg(out.join("skills.ext4")).output().unwrap();
        assert!(String::from_utf8_lossy(&ls.stdout).contains("run.sh"), "the image holds the skills");

        // Nothing chosen: an empty image, so corns remove what they had.
        assert_eq!(pack(&src, &BTreeSet::new(), &out).unwrap(), 0);
        assert!(out.join("skills.ext4").exists() && !cache.join("a").exists());
        fs::remove_dir_all(&t).ok();
    }
}
