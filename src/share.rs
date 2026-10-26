//! A folder on your computer, shared with a corn as a copy. Claude edits the copy; you review
//! each changed file and only approved ones are written back. The corn never sees the real folder.
//!
//! Git moves the files both ways, but never in your repository. Each corn has its own quarantine
//! repository (`share.git`). It reads your folder to make a snapshot, sends that to the corn as a
//! bundle, and receives the corn's work back as a bundle of new objects only. Your git settings,
//! hooks and filters are not used, and nothing from the corn is ever checked out on your side.
use crate::gate::{self, Event, Log};
use crate::sandbox::{connect, sock};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const IN_PORT: u32 = 1029;
const OUT_PORT: u32 = 1030;
/// Most a bundle may hold, either way: the snapshot after ignored files are left out, or the
/// corn's changes. The corn's disk is 8 GB.
const MAX_BUNDLE: u64 = 2 << 30;
const BASE: &str = "refs/popcorn/base";
const CORN: &str = "refs/popcorn/corn";

pub struct Share {
    pub host: PathBuf,
    /// The folder's name, also its place in the corn: ~/work/<name>.
    pub name: String,
}

#[derive(Clone, Copy, PartialEq, Debug)]
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
    /// The corn's version of the file, an object in the quarantine repository.
    oid: String,
}

pub fn get(dir: &Path) -> Option<Share> {
    let host = PathBuf::from(fs::read_to_string(dir.join("share")).ok()?.trim());
    let name = gate::safe_name(&host.file_name()?.to_string_lossy());
    (!name.is_empty() && !name.starts_with('.')).then_some(Share { host, name })
}

fn repo(dir: &Path) -> PathBuf {
    dir.join("share.git")
}

/// Git on the corn's quarantine repository. No system or global settings, no hooks, no
/// fsmonitor, no background maintenance, and paths are never read as patterns. Limits on memory,
/// CPU time and file size stop a hostile bundle from exhausting this computer.
fn git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", repo(dir))
        .env("GIT_DIR", repo(dir))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Popcorn")
        .env("GIT_AUTHOR_EMAIL", "popcorn@localhost")
        .env("GIT_COMMITTER_NAME", "Popcorn")
        .env("GIT_COMMITTER_EMAIL", "popcorn@localhost")
        .args(["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "-c", "gc.auto=0", "-c", "maintenance.auto=false"])
        .stdin(Stdio::null());
    unsafe {
        unsafe extern "C" {
            fn setrlimit(resource: i32, rlim: *const [u64; 2]) -> i32;
        }
        const RLIMIT_CPU: i32 = 0;
        const RLIMIT_FSIZE: i32 = 1;
        const RLIMIT_CORE: i32 = 4;
        const RLIMIT_AS: i32 = 9;
        std::os::unix::process::CommandExt::pre_exec(&mut c, || {
            for (r, v) in [(RLIMIT_CPU, 600), (RLIMIT_FSIZE, MAX_BUNDLE + (1 << 30)), (RLIMIT_CORE, 0), (RLIMIT_AS, 8 << 30)] {
                setrlimit(r, &[v, v]);
            }
            Ok(())
        });
    }
    c
}

fn run(c: &mut Command) -> Result<Vec<u8>, String> {
    let out = c.output().map_err(|e| format!("Popcorn cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(format!("git: {}", String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("failed")));
    }
    Ok(out.stdout)
}

fn line(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).trim().to_string()
}

fn old_image(e: String) -> String {
    format!("This corn cannot receive a shared folder. Its image may be older than this Popcorn: make a new corn. ({e})")
}

/// Snapshot your folder into a fresh quarantine repository: one commit with what git would track
/// (your .gitignore files, the repository's exclude file and your global ignore file apply), no
/// history. Contents are taken as they are, with no filters; links stay links. Returns the bundle
/// to send and the number of files.
fn pack(dir: &Path, host: &Path) -> Result<(PathBuf, usize), String> {
    let r = repo(dir);
    fs::remove_dir_all(&r).ok();
    run(git(dir).args(["init", "-q", "--bare"]))?;
    fs::write(r.join("info/attributes"), "* -text -eol -filter -ident -working-tree-encoding\n").map_err(|e| e.to_string())?;
    // Your repository's objects are read, never written, so unchanged files are not stored twice.
    if host.join(".git/objects").is_dir() {
        fs::write(r.join("objects/info/alternates"), host.join(".git/objects").to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        fs::copy(host.join(".git/info/exclude"), r.join("info/exclude")).ok();
    }
    // Other version control systems keep remotes, sometimes with passwords, in their folders.
    let mut exclude = fs::read(r.join("info/exclude")).unwrap_or_default();
    exclude.extend_from_slice(b"\n.hg/\n.svn/\n.bzr/\nCVS/\n_darcs/\n.arch-ids/\n{arch}/\n");
    fs::write(r.join("info/exclude"), exclude).map_err(|e| e.to_string())?;
    let index = r.join("send-index");
    let mut add = git(dir);
    add.env("GIT_WORK_TREE", host).env("GIT_INDEX_FILE", &index);
    if let Some(f) = global_ignore() {
        add.arg("-c").arg(format!("core.excludesFile={}", f.display()));
    }
    run(add.args(["add", "-A"]))?;
    let listed = run(git(dir).env("GIT_INDEX_FILE", &index).args(["ls-files", "-z"]))?;
    let listed: Vec<&[u8]> = listed.split(|&b| b == 0).filter(|p| !p.is_empty()).collect();
    // Popcorn reads git ignore rules only. A file that another system's rules hide would go to
    // the corn, so refuse until those rules are in .gitignore.
    for other in [".hgignore", ".bzrignore", ".cvsignore"] {
        if listed.iter().any(|p| p.rsplit(|&b| b == b'/').next() == Some(other.as_bytes())) {
            fs::remove_dir_all(&r).ok();
            return Err(format!(
                "{} has a {other} file. Popcorn reads only .gitignore. Copy its rules to .gitignore, add {other} to .gitignore, then share again.",
                host.display()
            ));
        }
    }
    let files = listed.len();
    let tree = line(run(git(dir).env("GIT_INDEX_FILE", &index).arg("write-tree"))?);
    fs::remove_file(&index).ok();
    let commit = line(run(git(dir).args(["commit-tree", &tree, "-m", "Shared from your computer"]))?);
    run(git(dir).args(["update-ref", BASE, &commit]))?;
    let bundle = r.join("send.bundle");
    run(git(dir).args(["bundle", "create", "-q"]).arg(&bundle).arg(BASE))?;
    if fs::metadata(&bundle).map(|m| m.len()).unwrap_or(0) > MAX_BUNDLE {
        fs::remove_file(&bundle).ok();
        return Err(format!("{} is too large, also without ignored files. The limit is 2 GB, compressed.", host.display()));
    }
    Ok((bundle, files))
}

/// Your global git ignore file, so files you ignore everywhere (such as .env) stay home too.
/// Only this one value is read from your git settings.
fn global_ignore() -> Option<PathBuf> {
    let out = Command::new("git").args(["config", "--global", "--path", "core.excludesFile"]).env("GIT_CONFIG_NOSYSTEM", "1").output().ok()?;
    let set = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let f = if set.is_empty() {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::home_dir().map(|h| h.join(".config")))?.join("git/ignore")
    } else {
        PathBuf::from(set)
    };
    f.is_file().then_some(f)
}

/// Refuse places that would hand a corn your secrets: your home folder, anything above it, and
/// folders holding SSH or GPG keys or settings.
pub fn refuse_secrets(p: &Path) -> Result<(), String> {
    let home = std::env::home_dir().and_then(|h| h.canonicalize().ok()).unwrap_or_default();
    if p == Path::new("/") || home.starts_with(p) {
        return Err(format!("{} contains your home folder. Select a project folder or specific files.", p.display()));
    }
    for secret in [".ssh", ".gnupg", ".aws", ".config"] {
        if p.is_dir() && p.join(secret).exists() {
            return Err(format!("{} contains {secret}. Select a folder without keys or settings.", p.display()));
        }
    }
    Ok(())
}

/// Most a copy into ~/inbox may hold. Checked before anything is packed.
const MAX_BYTES: u64 = 1 << 30;
const MAX_FILES: u64 = 20_000;

/// For copies into ~/inbox: refuse secrets, and anything over the size limits. Unlike a shared
/// folder, a copy takes every file, so every file counts. The scan never follows links.
pub fn check_source(path: &Path) -> Result<(), String> {
    let p = path.canonicalize().map_err(|e| e.to_string())?;
    refuse_secrets(&p)?;
    if p.is_file() {
        let size = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        return if size > MAX_BYTES { Err(format!("{} is larger than 1 GB.", p.display())) } else { Ok(()) };
    }
    let (mut bytes, mut files) = (0u64, 0u64);
    let mut stack = vec![p.clone()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).map_err(|e| e.to_string())?.flatten() {
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                stack.push(e.path());
            } else if t.is_file() {
                files += 1;
                bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
            if bytes > MAX_BYTES || files > MAX_FILES {
                return Err(format!("{} is too large. The limit is 1 GB or 20,000 files.", p.display()));
            }
        }
    }
    Ok(())
}

/// Copy the folder into the running corn at ~/work/<name>, replacing what was there.
/// `.git` and everything ignored stay on your computer.
pub fn send(dir: &Path, log: &Log, host: &Path) -> Result<usize, String> {
    let host = host.canonicalize().map_err(|e| e.to_string())?;
    if !host.is_dir() {
        return Err("Select a folder.".into());
    }
    refuse_secrets(&host)?;
    fs::write(dir.join("share"), host.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
    let share = get(dir).ok_or("Popcorn cannot use this folder name.")?;
    let (bundle, n) = pack(dir, &host)?;
    let size = fs::metadata(&bundle).map_err(|e| e.to_string())?.len();
    // Name and size first: the corn reads exactly that many bytes, and our side stays open for
    // its answer (socat ends a command soon after one side closes).
    let mut s = connect(&sock(dir), IN_PORT).map_err(old_image)?;
    // A corn that stops answering must not hang Popcorn. Unpacking a large folder takes a while.
    s.set_read_timeout(Some(std::time::Duration::from_secs(600))).ok();
    s.write_all(format!("{}\n{size}\n", share.name).as_bytes()).map_err(|e| e.to_string())?;
    std::io::copy(&mut fs::File::open(&bundle).map_err(|e| e.to_string())?, &mut s).map_err(|e| e.to_string())?;
    fs::remove_file(&bundle).ok();
    let mut answer = String::new();
    s.read_to_string(&mut answer).ok();
    if answer.trim() != "ok" {
        return Err(old_image(format!("the corn answered {:?}", answer.trim())));
    }
    gate::record(log, dir, Event::note(&format!("Shared {} ({n} files) at ~/work/{}", share.name, share.name)));
    Ok(n)
}

/// Forget the shared folder. Your folder is not touched; the corn keeps its copy in ~/work.
pub fn stop(dir: &Path, log: &Log) {
    let Some(share) = get(dir) else { return };
    fs::remove_file(dir.join("share")).ok();
    fs::remove_dir_all(repo(dir)).ok();
    gate::record(log, dir, Event::note(&format!("Stopped sharing {}", share.name)));
}

/// Ask the corn for its version of the folder and list what differs from what was shared.
pub fn pull(dir: &Path, log: &Log) -> Result<Vec<Change>, String> {
    let share = get(dir).ok_or("This corn has no shared folder.")?;
    let mut s = connect(&sock(dir), OUT_PORT).map_err(old_image)?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(120))).ok();
    s.write_all(format!("{}\n", share.name).as_bytes()).map_err(|e| e.to_string())?;
    receive(dir, log, &share, s)
}

/// Take the corn's bundle without trusting it: at most MAX_BUNDLE bytes, every object checked
/// (`fsck`), fetched into the quarantine repository only. Then compare it with what was shared.
fn receive(dir: &Path, log: &Log, share: &Share, r: impl Read) -> Result<Vec<Change>, String> {
    let bundle = repo(dir).join("pull.bundle");
    let mut f = OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&bundle).map_err(|e| e.to_string())?;
    let n = std::io::copy(&mut r.take(MAX_BUNDLE + 1), &mut f).map_err(|e| e.to_string())?;
    drop(f);
    if n > MAX_BUNDLE {
        fs::remove_file(&bundle).ok();
        return Err("The corn's changes are larger than 2 GB.".into());
    }
    let fetched = run(git(dir)
        .args(["-c", "transfer.fsckObjects=true", "-c", "fetch.fsckObjects=true", "fetch", "-q", "--no-tags", "--no-write-fetch-head"])
        .arg(&bundle)
        .arg(format!("+refs/popcorn/snapshot:{CORN}")));
    fs::remove_file(&bundle).ok();
    fetched.map_err(|e| format!("Popcorn refused the corn's changes. {e}"))?;
    let raw = run(git(dir).args(["diff-tree", "-r", "-z", "--no-renames", "--raw", BASE, CORN]))?;
    let (changes, skipped) = changes(&raw, &share.host);
    if skipped > 0 {
        let s = if skipped == 1 { "" } else { "s" };
        gate::record(log, dir, Event::note(&format!("Skipped {skipped} change{s} that are not plain files, such as links")));
    }
    Ok(changes)
}

/// Read `git diff-tree -z --raw` output. Only plain files with safe paths come through; links,
/// nested repositories and anything else are counted as skipped. Changes your folder already
/// has are left out.
fn changes(raw: &[u8], host: &Path) -> (Vec<Change>, usize) {
    const ZERO: &str = "0000000000000000000000000000000000000000";
    let plain = |m: &str| m == "100644" || m == "100755";
    let (mut out, mut skipped) = (Vec::new(), 0);
    let mut fields = raw.split(|&b| b == 0);
    while let (Some(meta), Some(path)) = (fields.next(), fields.next()) {
        let meta = String::from_utf8_lossy(meta);
        let m: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
        let [old_mode, new_mode, old, new, status] = m[..] else { break };
        let kind = match status {
            "A" if plain(new_mode) => Kind::Added,
            "M" if plain(old_mode) && plain(new_mode) => Kind::Changed,
            "D" if plain(old_mode) => Kind::Deleted,
            _ => {
                skipped += 1;
                continue;
            }
        };
        let Some(rel) = std::str::from_utf8(path).ok().filter(|r| safe_rel(r)) else {
            skipped += 1;
            continue;
        };
        let mine = host_oid(host, rel);
        let theirs = (kind != Kind::Deleted).then(|| new.to_string());
        if mine == theirs {
            continue;
        }
        let shared = (old != ZERO).then(|| old.to_string());
        out.push(Change { rel: rel.to_string(), kind, host_moved: mine != shared, oid: new.to_string() });
    }
    (out, skipped)
}

/// A relative path with plain parts only: no empty, ".", ".." or ".git" part, in any case.
fn safe_rel(rel: &str) -> bool {
    !rel.is_empty() && rel.split('/').all(|p| !p.is_empty() && p != "." && p != ".." && !p.eq_ignore_ascii_case(".git"))
}

/// The git object id of the regular file at `rel` in your folder, or None when there is none.
fn host_oid(root: &Path, rel: &str) -> Option<String> {
    let p = rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c));
    let m = fs::symlink_metadata(&p).ok().filter(|m| m.is_file())?;
    let mut f = fs::File::open(&p).ok()?;
    let mut h = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
    h.update(format!("blob {}\0", m.len()).as_bytes());
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match f.read(&mut buf).ok()? {
            0 => break,
            n => h.update(&buf[..n]),
        }
    }
    Some(h.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

/// A unified diff between what was shared and the corn's version.
pub fn diff(dir: &Path, c: &Change) -> String {
    let out = git(dir).args(["diff", "--no-color", "--no-ext-diff", "--no-textconv", BASE, CORN, "--", &c.rel]).output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    // ponytail: long diffs are cut at 400 lines.
    text.lines().skip_while(|l| !l.starts_with("@@") && !l.starts_with("Binary files")).take(400).collect::<Vec<_>>().join("\n")
}

/// The path inside your folder for `rel`, refusing anything that would leave it: no `..` or
/// `.git` part, and no symlinked directory or file along the way (a planted link must not
/// redirect a write).
fn target(root: &Path, rel: &str, create_dirs: bool) -> Result<PathBuf, String> {
    if !safe_rel(rel) {
        return Err(format!("Refused {rel}: the path is not safe."));
    }
    let parts: Vec<&str> = rel.split('/').collect();
    let mut p = root.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        p.push(part);
        let last = i == parts.len() - 1;
        match fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => return Err(format!("Refused {rel}: the path goes through a link.")),
            Ok(m) if !last && !m.is_dir() => return Err(format!("Refused {rel}: a file blocks the path.")),
            Ok(m) if last && !m.is_file() => return Err(format!("Refused {rel}: it is not a regular file.")),
            Err(_) if !last && create_dirs => fs::create_dir(&p).map_err(|e| e.to_string())?,
            _ => {}
        }
    }
    Ok(p)
}

/// Write one approved change to your folder. The new content streams from the quarantine
/// repository into a new file next to the target, which then replaces it.
pub fn apply(dir: &Path, log: &Log, c: &Change) -> Result<(), String> {
    let share = get(dir).ok_or("This corn has no shared folder.")?;
    let p = target(&share.host, &c.rel, c.kind != Kind::Deleted)?;
    match c.kind {
        Kind::Deleted => {
            if p.exists() {
                fs::remove_file(&p).map_err(|e| e.to_string())?;
            }
        }
        Kind::Added | Kind::Changed => {
            // Keep the file's existing permissions; new files are never executable.
            let mode = fs::metadata(&p).map(|m| m.permissions().mode() & 0o777).unwrap_or(0o644);
            let tmp = p.with_file_name(format!(".{}.popcorn-tmp", p.file_name().unwrap().to_string_lossy()));
            let mut f = OpenOptions::new().create_new(true).write(true).mode(mode).open(&tmp).map_err(|e| e.to_string())?;
            let mut cat = git(dir).args(["cat-file", "blob", &c.oid]).stdout(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
            let copied = std::io::copy(cat.stdout.as_mut().unwrap(), &mut f);
            let ok = cat.wait().is_ok_and(|s| s.success());
            if copied.is_err() || !ok {
                fs::remove_file(&tmp).ok();
                return Err(format!("Popcorn cannot read the corn's version of {}.", c.rel));
            }
            fs::rename(&tmp, &p).map_err(|e| e.to_string())?;
        }
    }
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

    /// Plays the corn with real git: clone the bundle, run `work`, send back a bundle made by `send`.
    fn corn_round(dir: &Path, guest: &Path, work: &str, send: &str) -> Vec<u8> {
        let script = format!(
            "set -e; git init -q -b work {g}; cd {g}; git fetch -q {d}/share.git/send.bundle refs/popcorn/base:refs/popcorn/base; \
             git checkout -q -B work refs/popcorn/base; {work}; {send}; git bundle create -q {g}/out.bundle refs/popcorn/snapshot ^refs/popcorn/base",
            g = guest.display(),
            d = dir.display()
        );
        let ok = Command::new("sh").arg("-c").arg(&script).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").status().unwrap().success();
        assert!(ok, "corn side failed: {script}");
        fs::read(guest.join("out.bundle")).unwrap()
    }

    #[test]
    fn shares_through_git_and_trusts_nothing_back() {
        let t = std::env::temp_dir().join(format!("popcorn-git-{}", std::process::id()));
        fs::remove_dir_all(&t).ok();
        let (dir, host) = (t.join("corn"), t.join("project"));
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(host.join("src")).unwrap();
        fs::create_dir_all(host.join("node_modules/x")).unwrap();
        fs::write(host.join(".gitignore"), "node_modules/\nsecret.env\n").unwrap();
        fs::write(host.join("secret.env"), "KEY=1").unwrap();
        fs::write(host.join("node_modules/x/big.js"), "x".repeat(4096)).unwrap();
        fs::write(host.join("a.txt"), "old\r\n").unwrap();
        fs::write(host.join("src/b.rs"), "fn b() {}\n").unwrap();
        fs::write(host.join("keep.txt"), "same").unwrap();
        let log: Log = Default::default();
        let share = Share { host: host.clone(), name: "project".into() };
        fs::write(dir.join("share"), host.to_string_lossy().as_bytes()).unwrap();

        // Other version control systems: their folders stay home, and their ignore rules are not
        // read, so a folder that would send one is refused until the rules are in .gitignore.
        for (d, f) in [(".hg", "hgrc"), (".svn", "wc.db"), ("sub/.bzr", "branch.conf"), ("CVS", "Root")] {
            fs::create_dir_all(host.join(d)).unwrap();
            fs::write(host.join(d).join(f), "https://user:password@example.com/").unwrap();
        }
        fs::write(host.join(".hgignore"), "secret.env\n").unwrap();
        let refused = pack(&dir, &host).err().unwrap_or_default();
        assert!(refused.contains(".hgignore"), "{refused}");
        fs::write(host.join(".gitignore"), "node_modules/\nsecret.env\n.hgignore\n").unwrap();

        let (_, files) = pack(&dir, &host).unwrap();
        assert_eq!(files, 4, "ignored files stay home: .gitignore, a.txt, keep.txt, src/b.rs");
        let snapshot = String::from_utf8_lossy(&run(git(&dir).args(["ls-tree", "-r", "--name-only", BASE])).unwrap()).into_owned();
        assert!(!snapshot.contains("secret.env") && !snapshot.contains("hgrc"), "{snapshot}");
        for d in [".hg/", ".svn/", ".bzr/", "CVS/"] {
            assert!(!snapshot.contains(d), "{d} stays home: {snapshot}");
        }

        // The corn edits, adds, deletes, plants a link to a host file and an executable.
        let commit = "git add -A && git -c user.name=c -c user.email=c@c commit -qm x && git update-ref refs/popcorn/snapshot HEAD";
        let work = "printf 'fn b() { todo!() }\\n' > src/b.rs; echo new > c.txt; rm a.txt; ln -s /etc/passwd link; printf '#!/bin/sh\\n' > run.sh; chmod +x run.sh";
        let bundle = corn_round(&dir, &t.join("guest"), work, commit);
        let changes = receive(&dir, &log, &share, &bundle[..]).unwrap();
        let mut seen: Vec<(String, Kind, bool)> = changes.iter().map(|c| (c.rel.clone(), c.kind, c.host_moved)).collect();
        seen.sort_by(|a, b| a.0.cmp(&b.0));
        let want = [("a.txt", Kind::Deleted, false), ("c.txt", Kind::Added, false), ("run.sh", Kind::Added, false), ("src/b.rs", Kind::Changed, false)];
        assert_eq!(seen, want.map(|(r, k, m)| (r.to_string(), k, m)), "the link is skipped, never offered");
        assert!(diff(&dir, changes.iter().find(|c| c.rel == "src/b.rs").unwrap()).contains("+fn b() { todo!() }"));

        // You change b.rs on your side meanwhile: the review says so.
        fs::write(host.join("src/b.rs"), "fn b() { mine() }\n").unwrap();
        let again = receive(&dir, &log, &share, &bundle[..]).unwrap();
        assert!(again.iter().any(|c| c.rel == "src/b.rs" && c.host_moved));
        for c in again.iter().filter(|c| c.rel != "a.txt") {
            apply(&dir, &log, c).unwrap();
        }
        assert_eq!(fs::read_to_string(host.join("src/b.rs")).unwrap(), "fn b() { todo!() }\n");
        assert_eq!(fs::read_to_string(host.join("c.txt")).unwrap(), "new\n");
        assert_eq!(fs::metadata(host.join("run.sh")).unwrap().permissions().mode() & 0o111, 0, "new files are never executable");
        assert!(host.join("a.txt").exists() && !host.join("link").exists());
        // What you applied is not offered again; the denied deletion still is.
        let left: Vec<String> = receive(&dir, &log, &share, &bundle[..]).unwrap().into_iter().map(|c| c.rel).collect();
        assert_eq!(left, ["a.txt"]);

        // A forged tree with a .git entry is refused when it arrives.
        let forge = "blob=$(echo evil | git hash-object -w --stdin); \
                     hooks=$(printf '100644 blob %s\\tpre-commit\\n' $blob | git mktree); \
                     dotgit=$(printf '040000 tree %s\\thooks\\n' $hooks | git mktree); \
                     root=$(printf '040000 tree %s\\t.git\\n' $dotgit | git mktree); \
                     c=$(git -c user.name=c -c user.email=c@c commit-tree $root -p HEAD -m x); git update-ref refs/popcorn/snapshot $c";
        let evil = corn_round(&dir, &t.join("guest2"), "true", forge);
        let refused = receive(&dir, &log, &share, &evil[..]).err().unwrap_or_default();
        assert!(refused.contains("hasDotgit"), "fsck refuses a .git path: {refused}");
        assert!(!host.join(".git").exists());
        // Garbage is refused too.
        assert!(receive(&dir, &log, &share, &b"not a bundle"[..]).is_err());
        fs::remove_dir_all(&t).ok();
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
        assert!(target(&root, ".git/hooks/pre-commit", true).is_err(), "never write into .git");
        assert!(target(&root, "sub/.GIT/config", true).is_err(), "in any case");
        fs::remove_dir_all(&root).ok();
    }
}
