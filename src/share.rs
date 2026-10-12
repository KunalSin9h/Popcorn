//! A folder on your computer, shared with a corn as a copy. Claude edits the copy; you review
//! each changed file and only approved ones are written back. The corn never sees the real folder.
use crate::gate::{self, Event, Log};
use crate::sandbox::connect;
use std::collections::{BTreeMap, BTreeSet};
use std::collections::hash_map::DefaultHasher;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

const SEND_PORT: u32 = 1027;
const PULL_PORT: u32 = 1028;

pub struct Share {
    pub host: PathBuf,
    /// The folder's name, also its place in the corn: ~/work/<name>.
    pub name: String,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Added,
    Changed,
    Deleted,
}

#[derive(Clone)]
pub struct Change {
    pub rel: String,
    pub kind: Kind,
    /// The file on your side changed since it was shared; approving overwrites that.
    pub host_moved: bool,
}

fn digest(bytes: &[u8]) -> String {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    format!("{:016x}", h.finish())
}

pub fn get(dir: &Path) -> Option<Share> {
    let host = PathBuf::from(fs::read_to_string(dir.join("share")).ok()?.trim());
    let name = gate::safe_name(&host.file_name()?.to_string_lossy());
    (!name.is_empty() && !name.starts_with('.')).then_some(Share { host, name })
}

/// What was sent last: path inside the folder -> content digest.
fn manifest(dir: &Path) -> BTreeMap<String, String> {
    fs::read_to_string(dir.join("share.manifest"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t').map(|(h, p)| (p.to_string(), h.to_string())))
        .collect()
}

fn save_manifest(dir: &Path, m: &BTreeMap<String, String>) {
    let text: String = m.iter().map(|(p, h)| format!("{h}\t{p}\n")).collect();
    fs::write(dir.join("share.manifest"), text).ok();
}

/// Most a single share or copy may hold. Checked before anything is packed.
const MAX_BYTES: u64 = 1 << 30;
const MAX_FILES: u64 = 20_000;

/// Refuse sources that would hand a corn your secrets or blow up memory: your home folder,
/// anything above it, folders holding SSH or GPG keys, and anything over the size limits.
/// The scan never follows links and skips `.git`.
pub fn check_source(path: &Path) -> Result<(), String> {
    let p = path.canonicalize().map_err(|e| e.to_string())?;
    let home = std::env::home_dir().and_then(|h| h.canonicalize().ok()).unwrap_or_default();
    if p == Path::new("/") || home.starts_with(&p) {
        return Err(format!("{} holds your whole home folder. Pick a project folder or specific files.", p.display()));
    }
    if p.is_file() {
        let size = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        return if size > MAX_BYTES { Err(format!("{} is over 1 GB.", p.display())) } else { Ok(()) };
    }
    for secret in [".ssh", ".gnupg", ".aws", ".config"] {
        if p.join(secret).exists() {
            return Err(format!("{} contains {secret}. Pick a folder without your keys or settings in it.", p.display()));
        }
    }
    let (mut bytes, mut files) = (0u64, 0u64);
    let mut stack = vec![p.clone()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).map_err(|e| e.to_string())?.flatten() {
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() && e.file_name() != ".git" {
                stack.push(e.path());
            } else if t.is_file() {
                files += 1;
                bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
            if bytes > MAX_BYTES || files > MAX_FILES {
                return Err(format!("{} is too big for a corn (over 1 GB or 20,000 files). Pick a smaller folder.", p.display()));
            }
        }
    }
    Ok(())
}

/// Copy the folder into the running corn at ~/work/<name>, replacing what was there.
/// `.git` and everything the folder's .gitignore files exclude stay on your computer.
pub fn send(dir: &Path, log: &Log, host: &Path) -> Result<usize, String> {
    let host = host.canonicalize().map_err(|e| e.to_string())?;
    if !host.is_dir() {
        return Err("Pick a folder".into());
    }
    check_source(&host)?;
    fs::write(dir.join("share"), host.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
    let share = get(dir).ok_or("That folder name can't be used")?;
    // Pack to a file on disk, never into memory. Links are stored as links, never followed,
    // so nothing outside the folder is read.
    let snapshot = dir.join("snapshot.tar");
    let out = Command::new("tar")
        .args(["-c", "--exclude-vcs", "--exclude-vcs-ignores", "-f"])
        .arg(&snapshot)
        .arg("-C")
        .arg(&host)
        .arg(".")
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        fs::remove_file(&snapshot).ok();
        return Err(format!("Could not pack the folder: {}", String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("")));
    }
    let mut m = BTreeMap::new();
    gate::read_tar(fs::File::open(&snapshot).map_err(|e| e.to_string())?, |parts, body| {
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        m.insert(parts.join("/"), digest(&bytes));
        Ok(())
    })?;
    let mut s = connect(&dir.join("v.sock"), SEND_PORT)?;
    s.write_all(format!("{}\n", share.name).as_bytes()).map_err(|e| e.to_string())?;
    std::io::copy(&mut fs::File::open(&snapshot).map_err(|e| e.to_string())?, &mut s).map_err(|e| e.to_string())?;
    fs::remove_file(&snapshot).ok();
    s.shutdown(std::net::Shutdown::Write).ok();
    let mut sink = Vec::new();
    s.read_to_end(&mut sink).ok();
    let n = m.len();
    save_manifest(dir, &m);
    gate::record(log, dir, Event::note(&format!("Shared {} ({n} files) at ~/work/{}", share.name, share.name)));
    Ok(n)
}

fn staging(dir: &Path) -> PathBuf {
    dir.join("staging")
}

/// Bring the corn's copy back into staging (on your computer, inside the corn's folder) and list
/// what differs from what was shared.
pub fn pull(dir: &Path) -> Result<Vec<Change>, String> {
    let share = get(dir).ok_or("No folder is shared with this corn")?;
    let stage = staging(dir);
    fs::remove_dir_all(&stage).ok();
    fs::create_dir_all(&stage).map_err(|e| e.to_string())?;
    let mut s = connect(&dir.join("v.sock"), PULL_PORT)?;
    s.write_all(format!("{}\n", share.name).as_bytes()).map_err(|e| e.to_string())?;
    let mut now = BTreeMap::new();
    gate::read_tar(&mut s, |parts, body| {
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        let rel = parts.join("/");
        let path = parts.iter().fold(stage.clone(), |p, c| p.join(c));
        fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
        OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&path).and_then(|mut f| f.write_all(&bytes)).map_err(|e| e.to_string())?;
        now.insert(rel, digest(&bytes));
        Ok(())
    })?;
    let sent = manifest(dir);
    let host_digest = |rel: &str| fs::read(share.host.join(rel)).ok().map(|b| digest(&b));
    let mut out = Vec::new();
    for rel in sent.keys().chain(now.keys()).collect::<BTreeSet<_>>() {
        let kind = match (sent.get(rel), now.get(rel)) {
            (None, Some(_)) => Kind::Added,
            (Some(a), Some(b)) if a != b => Kind::Changed,
            (Some(_), None) => Kind::Deleted,
            _ => continue,
        };
        let host_moved = host_digest(rel) != sent.get(rel).cloned();
        out.push(Change { rel: rel.clone(), kind, host_moved });
    }
    Ok(out)
}

/// A unified diff between your file and the corn's version.
pub fn diff(dir: &Path, c: &Change) -> String {
    let Some(share) = get(dir) else { return String::new() };
    let mine = share.host.join(&c.rel);
    let theirs = staging(dir).join(&c.rel);
    let (a, b) = match c.kind {
        Kind::Added => (PathBuf::from("/dev/null"), theirs),
        Kind::Deleted => (mine, PathBuf::from("/dev/null")),
        Kind::Changed => (mine, theirs),
    };
    let out = Command::new("diff").args(["-u", "--label", "yours", "--label", "corn"]).arg(a).arg(b).output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    // ponytail: long diffs are cut; open the staging copy for the rest.
    text.lines().skip(2).take(400).collect::<Vec<_>>().join("\n")
}

/// The path inside your folder for `rel`, refusing anything that would leave it: no `..`, and no
/// symlinked directory or file along the way (a planted link must not redirect a write).
fn target(root: &Path, rel: &str, create_dirs: bool) -> Result<PathBuf, String> {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
        return Err(format!("Refused unsafe path {rel}"));
    }
    let mut p = root.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        p.push(part);
        let last = i == parts.len() - 1;
        match fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => return Err(format!("Refused {rel}: it goes through a link")),
            Ok(m) if !last && !m.is_dir() => return Err(format!("Refused {rel}: a file is in the way")),
            Ok(m) if last && !m.is_file() => return Err(format!("Refused {rel}: not a regular file")),
            Err(_) if !last && create_dirs => fs::create_dir(&p).map_err(|e| e.to_string())?,
            _ => {}
        }
    }
    Ok(p)
}

/// Write one approved change to your folder.
pub fn apply(dir: &Path, log: &Log, c: &Change) -> Result<(), String> {
    let share = get(dir).ok_or("No folder is shared with this corn")?;
    let mut m = manifest(dir);
    match c.kind {
        Kind::Deleted => {
            let p = target(&share.host, &c.rel, false)?;
            if p.exists() {
                fs::remove_file(&p).map_err(|e| e.to_string())?;
            }
            m.remove(&c.rel);
        }
        Kind::Added | Kind::Changed => {
            let bytes = fs::read(staging(dir).join(&c.rel)).map_err(|e| e.to_string())?;
            let p = target(&share.host, &c.rel, true)?;
            // Keep the file's existing permissions; new files are never executable.
            let mode = fs::metadata(&p).map(|m| m.permissions().mode() & 0o777).unwrap_or(0o644);
            let tmp = p.with_file_name(format!(".{}.popcorn-tmp", p.file_name().unwrap().to_string_lossy()));
            OpenOptions::new().create_new(true).write(true).mode(mode).open(&tmp).and_then(|mut f| f.write_all(&bytes)).map_err(|e| e.to_string())?;
            fs::rename(&tmp, &p).map_err(|e| e.to_string())?;
            m.insert(c.rel.clone(), digest(&bytes));
        }
    }
    save_manifest(dir, &m);
    let verb = match c.kind {
        Kind::Added => "Added",
        Kind::Changed => "Updated",
        Kind::Deleted => "Deleted",
    };
    gate::record(log, dir, Event::note(&format!("{verb} {} in {}", c.rel, share.name)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_home_and_secret_folders() {
        let home = std::env::home_dir().unwrap();
        assert!(check_source(&home).is_err(), "home itself");
        assert!(check_source(Path::new("/")).is_err());
        assert!(check_source(home.parent().unwrap()).is_err(), "anything above home");
        let p = std::env::temp_dir().join(format!("popcorn-src-{}", std::process::id()));
        fs::create_dir_all(p.join(".ssh")).unwrap();
        assert!(check_source(&p).is_err(), "a folder with keys");
        fs::remove_dir_all(p.join(".ssh")).unwrap();
        fs::write(p.join("a.txt"), "x").unwrap();
        assert!(check_source(&p).is_ok());
        assert!(check_source(&p.join("a.txt")).is_ok());
        fs::remove_dir_all(&p).ok();
    }

    #[test]
    fn writes_stay_inside_the_folder() {
        let root = std::env::temp_dir().join(format!("popcorn-share-{}", std::process::id()));
        fs::create_dir_all(root.join("src")).unwrap();
        std::os::unix::fs::symlink("/etc", root.join("etc-link")).unwrap();
        assert!(target(&root, "src/main.rs", true).is_ok());
        assert!(target(&root, "new/dir/file.txt", true).is_ok() && root.join("new/dir").is_dir());
        assert!(target(&root, "../escape", true).is_err());
        assert!(target(&root, "etc-link/passwd", true).is_err(), "never write through a link");
        assert!(target(&root, "etc-link", true).is_err());
        fs::remove_dir_all(&root).ok();
    }
}
