//! `popcorn landlock`, the third wall around Firecracker: only the named files, no TCP, no
//! signals out. Runs the real binary on ordinary programs from /usr.
use std::process::Command;

fn run(rules: &[&str], cmd: &str) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_popcorn")).arg("landlock").args(rules).args(["--", "/usr/bin/bash", "-c", cmd]).output().unwrap();
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn landlock_allows_only_what_is_named() {
    let (ok, out) = run(&["x:/usr", "ro:/etc/hostname"], "cat /etc/hostname");
    assert!(ok, "a named file can be read: {out}");
    let (ok, out) = run(&["x:/usr", "ro:/etc/hostname"], "cat /etc/passwd");
    assert!(!ok && out.contains("Permission denied"), "any other file is refused: {out}");
    let home = std::env::var("HOME").unwrap();
    let (ok, out) = run(&["x:/usr"], &format!("ls {home}"));
    assert!(!ok && out.contains("Permission denied"), "your home folder is refused: {out}");
    let tmp = std::env::temp_dir().join(format!("popcorn-landlock-{}", std::process::id()));
    let (ok, _) = run(&["x:/usr", "ro:/tmp"], &format!("touch {}", tmp.display()));
    assert!(!ok && !tmp.exists(), "read-only means no writes");
    let (ok, out) = run(&["x:/usr"], "exec 3<>/dev/tcp/127.0.0.1/9");
    assert!(!ok && out.contains("Permission denied"), "TCP is closed: {out}");
    let mut outside = Command::new("sleep").arg("30").spawn().unwrap();
    let (ok, _) = run(&["x:/usr"], &format!("kill -0 {}", outside.id()));
    outside.kill().ok();
    assert!(!ok, "signals cannot leave");
    let out = Command::new(env!("CARGO_BIN_EXE_popcorn")).args(["landlock", "ro:/usr", "--", "/usr/bin/true"]).output().unwrap();
    assert!(!out.status.success(), "a program that is not named cannot run");
}
