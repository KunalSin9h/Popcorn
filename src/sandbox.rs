//! Cases: one disposable Firecracker microVM each. The golden image is shared read-only; every
//! change a case makes lands on its own disk. The VM has no network card, only vsock to us.
use crate::gate::{self, Event, Log, Policy};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

const CASE_DISK: u64 = 8 << 30; // sparse, grows as used
pub const SHELL_PORT: u32 = 1024;
pub const FILES_PORT: u32 = 1025;
pub const OUTBOX_PORT: u32 = 1026;

pub fn home() -> PathBuf {
    std::env::var_os("POPCORN_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::home_dir().unwrap_or_default().join(".local/share/popcorn"))
}

/// What the shell inside a case calls you: `name@popcorn`. Saved in ~/.config/popcorn/user.
pub fn user_name() -> Option<String> {
    if let Ok(u) = std::env::var("POPCORN_USER") {
        return Some(u).filter(|u| valid_user(u));
    }
    let p = std::env::home_dir()?.join(".config/popcorn/user");
    fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| valid_user(s))
}

/// Linux account name rules, kept strict: it goes on the kernel command line and into usermod.
pub fn valid_user(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_lowercase()) && c.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_') && s.len() <= 24 && s != "root"
}

/// Turn what someone typed ("Kunal Singh") into an account name ("kunal-singh").
pub fn to_user(name: &str) -> String {
    let s: String = name.trim().to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    let s = s.trim_start_matches(|c: char| c.is_ascii_digit() || c == '-').chars().take(24).collect::<String>();
    s.trim_end_matches('-').to_string()
}

pub fn save_user_name(name: &str) -> Result<String, String> {
    let u = to_user(name);
    if !valid_user(&u) {
        return Err("Use letters to start, then letters, numbers or dashes.".into());
    }
    let p = std::env::home_dir().ok_or("no home folder")?.join(".config/popcorn/user");
    fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(p, &u).map_err(|e| e.to_string())?;
    Ok(u)
}

pub fn golden() -> PathBuf {
    home().join("golden.ext4")
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    /// Every outside host waits for your answer. Claude always works, through its own channel.
    Ask,
    /// Only Claude. Every other attempt is refused and logged.
    Sealed,
}

pub struct Case {
    pub name: String,
    pub dir: PathBuf,
    pub mode: Mode,
    /// Shared with the gate: mode, allowlist and pending questions apply to a running case at once.
    pub policy: Arc<Policy>,
    pub log: Log,
    pub started: Option<std::time::Instant>,
    vm: Option<Child>,
}

impl Case {
    pub fn list() -> Vec<Case> {
        let mut out: Vec<Case> = fs::read_dir(home().join("cases"))
            .map(|rd| rd.flatten().filter(|e| e.path().join("disk.ext4").exists()).map(|e| Case::open(e.path())).collect())
            .unwrap_or_default();
        // Oldest first, so tabs keep the order they were opened in.
        out.sort_by_key(|c| fs::metadata(&c.dir).and_then(|m| m.created()).ok());
        out
    }

    fn open(dir: PathBuf) -> Case {
        let mode = match fs::read_to_string(dir.join("mode")).unwrap_or_default().trim() {
            "sealed" | "detonation" => Mode::Sealed,
            _ => Mode::Ask,
        };
        let name = case_name(&dir);
        let log = Arc::new(Mutex::new(gate::read_log(&dir)));
        let policy = Policy::load(&dir, mode);
        Case { name, dir, mode, policy, log, started: None, vm: None }
    }

    /// A new case: a fresh, empty, sparse disk. Creating one takes well under a second.
    pub fn create() -> Result<Case, String> {
        let root = home().join("cases");
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let n = (1..).find(|n| !root.join(format!("case-{n}")).exists() && !root.join(format!("corn-{n}")).exists()).unwrap();
        let dir = root.join(format!("corn-{n}"));
        fs::create_dir(&dir).map_err(|e| e.to_string())?;
        let disk = dir.join("disk.ext4");
        fs::File::create(&disk).and_then(|f| f.set_len(CASE_DISK)).map_err(|e| e.to_string())?;
        let ok = Command::new("mkfs.ext4").args(["-q", "-F", "-L", "case"]).arg(&disk).status().map_err(|e| e.to_string())?;
        if !ok.success() {
            return Err("mkfs.ext4 failed".into());
        }
        Ok(Case::open(dir))
    }

    pub fn rename(&mut self, title: &str) {
        let t = title.trim();
        if !t.is_empty() {
            self.name = t.chars().take(48).collect();
            fs::write(self.dir.join("title"), &self.name).ok();
        }
    }

    pub fn has_title(&self) -> bool {
        self.dir.join("title").exists()
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        *self.policy.mode.lock().unwrap() = mode;
        fs::write(self.dir.join("mode"), if mode == Mode::Sealed { "sealed" } else { "ask" }).ok();
    }

    pub fn running(&mut self) -> bool {
        match &mut self.vm {
            Some(c) => match c.try_wait() {
                Ok(None) => true,
                _ => {
                    self.vm = None;
                    self.started = None;
                    false
                }
            },
            None => false,
        }
    }

    fn sock(&self) -> PathBuf {
        self.dir.join("v.sock")
    }

    pub fn start(&mut self) -> Result<(), String> {
        if self.running() {
            return Ok(());
        }
        if !golden().exists() {
            return Err("The golden image is not built yet".into());
        }
        // No token yet is fine: the corn runs, and Claude inside gets a clear "not connected" error.
        let auth = gate::credential().map(|c| c.kind).unwrap_or(gate::Auth::Key);
        for p in [self.sock(), gate::port_path(&self.sock(), 1080), gate::port_path(&self.sock(), 1081)] {
            fs::remove_file(p).ok();
        }
        // The gate must be listening before the guest dials out.
        gate::serve(&self.sock(), self.policy.clone(), self.log.clone(), self.dir.clone())?;
        let config = format!(
            r#"{{
  "boot-source": {{"kernel_image_path": "{kernel}", "boot_args": "console=ttyS0 reboot=k panic=1 pci=off quiet init=/sbin/popcorn-init popcorn.auth={auth} popcorn.user={user}"}},
  "drives": [
    {{"drive_id": "golden", "path_on_host": "{golden}", "is_root_device": true, "is_read_only": true}},
    {{"drive_id": "case", "path_on_host": "{disk}", "is_root_device": false, "is_read_only": false}}
  ],
  "machine-config": {{"vcpu_count": 2, "mem_size_mib": 2048}},
  "vsock": {{"guest_cid": 3, "uds_path": "{sock}"}}
}}"#,
            kernel = home().join("vmlinux").display(),
            auth = auth.as_str(),
            user = user_name().unwrap_or_else(|| "analyst".into()),
            golden = golden().display(),
            disk = self.dir.join("disk.ext4").display(),
            sock = self.sock().display(),
        );
        fs::write(self.dir.join("vm.json"), config).map_err(|e| e.to_string())?;
        let console = fs::File::create(self.dir.join("console.log")).map_err(|e| e.to_string())?;
        // ponytail: plain firecracker as this user; wrap in `jailer` (needs root) for a second wall.
        let child = Command::new(home().join("bin/firecracker"))
            .args(["--no-api", "--config-file"])
            .arg(self.dir.join("vm.json"))
            .stdin(Stdio::null())
            .stdout(console.try_clone().map_err(|e| e.to_string())?)
            .stderr(console)
            .spawn()
            .map_err(|e| format!("could not start firecracker: {e}"))?;
        self.vm = Some(child);
        self.started = Some(std::time::Instant::now());
        gate::record(&self.log, &self.dir, Event::note("Corn started in its own microVM"));
        Ok(())
    }

    pub fn stop(&mut self) {
        self.started = None;
        if let Some(mut c) = self.vm.take() {
            c.kill().ok();
            c.wait().ok();
            gate::record(&self.log, &self.dir, Event::note("Corn stopped"));
        }
    }

    pub fn delete(mut self) {
        self.stop();
        fs::remove_dir_all(&self.dir).ok();
    }

    /// Open a terminal window with a shell inside the case.
    pub fn open_terminal(&self) -> Result<(), String> {
        let me = std::env::current_exe().map_err(|e| e.to_string())?;
        open_in_terminal(&[me.as_os_str(), "attach".as_ref(), self.dir.as_os_str()])
    }
}

/// Run a command in a new terminal window. Each terminal takes the command differently;
/// the first one installed wins.
pub fn open_in_terminal(cmd: &[&std::ffi::OsStr]) -> Result<(), String> {
    const TERMINALS: &[(&str, &[&str])] = &[
        ("ptyxis", &["--new-window", "--"]),
        ("gnome-terminal", &["--"]),
        ("kgx", &["--"]),
        ("konsole", &["-e"]),
        ("alacritty", &["-e"]),
        ("kitty", &[]),
        ("x-terminal-emulator", &["-e"]),
        ("xterm", &["-e"]),
    ];
    let path = std::env::var_os("PATH").unwrap_or_default();
    let (term, args) = TERMINALS.iter().find(|(t, _)| std::env::split_paths(&path).any(|d| d.join(t).exists())).ok_or("No terminal app found")?;
    Command::new(term).args(*args).args(cmd).spawn().map_err(|e| format!("Could not open {term}: {e}"))?;
    Ok(())
}

/// Copy a file or folder from the host into ~/inbox in a running case, as a tar stream over vsock.
pub fn send(dir: &Path, log: &Log, path: &Path) -> Result<(), String> {
    crate::share::check_source(path)?;
    let (parent, name) = (path.parent().ok_or("bad path")?, path.file_name().ok_or("bad path")?);
    let mut tar = Command::new("tar").arg("-c").arg("-C").arg(parent).arg(name).stdout(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let mut s = connect(&dir.join("v.sock"), FILES_PORT)?;
    std::io::copy(tar.stdout.as_mut().unwrap(), &mut s).map_err(|e| e.to_string())?;
    tar.wait().ok();
    s.shutdown(std::net::Shutdown::Write).ok();
    let mut sink = Vec::new();
    s.read_to_end(&mut sink).ok();
    gate::record(log, dir, Event::note(&format!("Copied {} into ~/inbox", name.to_string_lossy())));
    Ok(())
}

/// Pull ~/outbox from a running case into `<case>/exported/<n>`, a quarantine folder on the host.
pub fn export(dir: &Path, log: &Log) -> Result<(PathBuf, usize), String> {
    // Keep our side open: socat ends the guest command shortly after it sees our EOF.
    let mut s = connect(&dir.join("v.sock"), OUTBOX_PORT)?;
    let root = dir.join("exported");
    let n = (1..).find(|n| !root.join(n.to_string()).exists()).unwrap();
    let dest = root.join(n.to_string());
    let files = gate::import(&mut s, &dest)?;
    let msg = format!("Exported {} file{} from ~/outbox to quarantine", files.len(), if files.len() == 1 { "" } else { "s" });
    gate::record(log, dir, Event::note(&msg));
    Ok((dest, files.len()))
}

/// A title you gave the corn (or the first file dropped in), else "Corn N" from its folder.
pub fn case_name(dir: &Path) -> String {
    fs::read_to_string(dir.join("title")).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).unwrap_or_else(|| {
        dir.file_name().map(|n| n.to_string_lossy().replace("case-", "Corn ").replace("corn-", "Corn ")).unwrap_or_default()
    })
}

/// Host-initiated vsock connection: Firecracker's handshake on the VM's unix socket.
pub fn connect(sock: &Path, port: u32) -> Result<UnixStream, String> {
    let mut s = UnixStream::connect(sock).map_err(|e| format!("The corn is not running ({e})"))?;
    s.write_all(format!("CONNECT {port}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(s.try_clone().map_err(|e| e.to_string())?).read_line(&mut line).map_err(|e| e.to_string())?;
    if !line.starts_with("OK") {
        return Err(format!("The corn refused the connection: {line}"));
    }
    Ok(s)
}

/// `popcorn attach <case dir>`: bridge this terminal to the shell inside the case.
pub fn attach(dir: &Path) -> ! {
    let sock = dir.join("v.sock");
    let mut s = match (1..=40).find_map(|_| connect(&sock, SHELL_PORT).ok().or_else(|| {
        std::thread::sleep(std::time::Duration::from_millis(250));
        None
    })) {
        Some(s) => s,
        None => {
            eprintln!("Could not reach the corn. Is it running?");
            std::thread::sleep(std::time::Duration::from_secs(4));
            std::process::exit(1);
        }
    };
    // Tell the guest shell what terminal it is talking to and how big it is, then clear the
    // screen. The leading space keeps this line out of the shell history.
    let term = std::env::var("TERM").ok().filter(|t| !t.is_empty() && t != "dumb").unwrap_or_else(|| "xterm-256color".into());
    let size = Command::new("stty").arg("size").stdin(Stdio::inherit()).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    // Make this window unmistakable: title, background, cursor (standard escape codes; a terminal
    // that doesn't know one ignores it), and a prompt badge that works everywhere.
    let name: String = case_name(dir).chars().filter(|c| c.is_alphanumeric() || " ._-".contains(*c)).collect();
    {
        let mut o = std::io::stdout();
        // A full "lab" theme: text, background, cursor and the 16 ANSI colors.
        const PALETTE: [&str; 16] = [
            "#1b2a24", "#e36a5c", "#6fbf98", "#f0be2c", "#6fa8d6", "#c49ad8", "#5fc4b8", "#cfdcd4",
            "#3d5249", "#f08a7e", "#8fd8b2", "#f6d264", "#94c2e8", "#d8b6e8", "#86dccf", "#eef5f0",
        ];
        let palette: String = PALETTE.iter().enumerate().map(|(i, c)| format!("\x1b]4;{i};{c}\x07")).collect();
        let _ = write!(o, "\x1b]0;🍿 {name}\x07\x1b]10;#d9e6de\x07\x1b]11;#13201b\x07\x1b]12;#f0be2c\x07{palette}");
        let _ = o.flush();
    }
    let ps1 = format!(r"\[\e[1;30;43m\] {name} \[\e[0m\] \[\e[32m\]\u@popcorn\[\e[0m\]:\[\e[36m\]\w\[\e[0m\]\$ ");
    let mut setup = format!(" export TERM={term} PS1='{ps1}'");
    if let Some((rows, cols)) = size.as_deref().and_then(|s| s.split_once(' ')).filter(|(r, c)| *r != "0" && *c != "0") {
        // ponytail: size is set once; resizing the window later does not reach the guest.
        setup.push_str(&format!("; stty rows {rows} cols {cols}"));
    }
    setup.push_str("; clear\n");
    let _ = s.write_all(setup.as_bytes());
    Command::new("stty").args(["raw", "-echo"]).stdin(Stdio::inherit()).status().ok();
    let mut to_guest = s.try_clone().unwrap();
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        let mut stdin = std::io::stdin().lock();
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || to_guest.write_all(&buf[..n]).is_err() {
                break;
            }
        }
    });
    // Unbuffered both ways: every keystroke's echo reaches the screen at once.
    let mut out = std::io::stdout().lock();
    let mut buf = [0u8; 8192];
    while let Ok(n) = s.read(&mut buf) {
        if n == 0 || out.write_all(&buf[..n]).and_then(|_| out.flush()).is_err() {
            break;
        }
    }
    // Give the terminal its own colors back.
    let _ = out.write_all(b"\x1b]104\x07\x1b]110\x07\x1b]111\x07\x1b]112\x07");
    let _ = out.flush();
    drop(out);
    Command::new("stty").arg("sane").stdin(Stdio::inherit()).status().ok();
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_become_safe_account_names() {
        assert_eq!(to_user("Kunal Singh"), "kunal-singh");
        assert_eq!(to_user("  42 Ana_Maria!! "), "ana-maria");
        assert_eq!(to_user("$(reboot)"), "reboot");
        assert!(valid_user("kunal") && !valid_user("root") && !valid_user("9lives") && !valid_user("a b") && !valid_user(""));
    }
    use std::time::{Duration, Instant};

    /// Run one shell command in the case and return its output (until the marker comes back).
    fn sh(s: &mut UnixStream, cmd: &str, wait: Duration) -> String {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let mark = format!("__done_{}__", N.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        s.write_all(format!("{cmd}; echo {mark}\n").as_bytes()).unwrap();
        s.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        let (mut out, start, mut buf) = (String::new(), Instant::now(), [0u8; 4096]);
        // The marker alone on a line is the command's end; the echoed command line has more on it.
        while start.elapsed() < wait && !out.lines().any(|l| l.trim() == mark) {
            if let Ok(n) = s.read(&mut buf) {
                out.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        }
        out
    }

    /// Boots a real microVM. Needs the golden image and KVM: `cargo test --release -- --ignored`.
    #[test]
    #[ignore]
    fn case_boots_isolated_and_gated() {
        unsafe {
            std::env::set_var("POPCORN_CLAUDE_KEY", "sk-ant-api03-fake-key-for-tests");
            std::env::set_var("POPCORN_USER", "tester");
        }
        let mut case = Case::create().unwrap();
        case.start().unwrap();
        let sock = case.dir.join("v.sock");
        let mut s = (0..80).find_map(|_| connect(&sock, SHELL_PORT).ok().or_else(|| { std::thread::sleep(Duration::from_millis(250)); None })).expect("shell");
        let net = sh(&mut s, "ls /sys/class/net", Duration::from_secs(10));
        eprintln!("--- interfaces:\n{net}");
        assert!(!net.contains("eth0"), "no network card");

        // Every host is held for a decision. Answer like you would in the UI: allow the npm
        // registry, deny anything else.
        let policy = case.policy.clone();
        std::thread::spawn(move || {
            for _ in 0..6000 {
                let next = { let mut v = policy.asks.lock().unwrap(); if v.is_empty() { None } else { Some(v.remove(0)) } };
                if let Some(a) = next {
                    let ok = a.target.starts_with("registry.npmjs.org");
                    a.answer(if ok { gate::Answer::Once } else { gate::Answer::Deny });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let denied = sh(&mut s, "curl -sS -m 20 https://example.com -o /dev/null; echo rc=$?", Duration::from_secs(30));
        eprintln!("--- example.com:\n{denied}");
        let npm = sh(&mut s, "cd ~/work && npm view left-pad version --no-update-notifier 2>&1 | tail -2", Duration::from_secs(60));
        eprintln!("--- npm:\n{npm}");
        let api = sh(&mut s, "curl -sS -m 20 -o /dev/null -w 'api=%{http_code}' $ANTHROPIC_BASE_URL/v1/models -H 'anthropic-version: 2023-06-01'", Duration::from_secs(30));
        eprintln!("--- claude api:\n{api}");
        let curl_api = "curl -sS -m 20 -o /dev/null -w 'api=%{http_code}' $ANTHROPIC_BASE_URL/v1/models -H 'anthropic-version: 2023-06-01'";
        case.set_mode(Mode::Sealed);
        let sealed = sh(&mut s, curl_api, Duration::from_secs(30));
        case.set_mode(Mode::Ask);
        unsafe { std::env::set_var("POPCORN_CLAUDE_KEY", "") };
        let no_token = sh(&mut s, curl_api, Duration::from_secs(30));
        unsafe { std::env::set_var("POPCORN_CLAUDE_KEY", "sk-ant-api03-fake-key-for-tests") };
        let env = sh(&mut s, "env | grep -c sk-ant-api03-fake || true", Duration::from_secs(10));
        sh(&mut s, "echo '# report' > ~/outbox/report.md; ln -s /etc/passwd ~/outbox/link", Duration::from_secs(10));
        let (dest, n) = export(&case.dir, &case.log).unwrap();
        let exported = dest.join("report.md").exists();
        // A shared folder: secrets and .git stay home; edits come back only when applied.
        let host = std::env::temp_dir().join(format!("popcorn-shared-{}", std::process::id()));
        fs::create_dir_all(host.join("src")).unwrap();
        fs::create_dir_all(host.join(".git")).unwrap();
        fs::write(host.join(".git/config"), "token").unwrap();
        fs::write(host.join(".gitignore"), "secret.env\n").unwrap();
        fs::write(host.join("secret.env"), "API_KEY=1").unwrap();
        fs::write(host.join("a.txt"), "old").unwrap();
        fs::write(host.join("src/b.rs"), "fn b() {}\n").unwrap();
        let sent = crate::share::send(&case.dir, &case.log, &host).unwrap();
        let name = host.file_name().unwrap().to_string_lossy().to_string();
        let inside = sh(&mut s, &format!("cd ~/work/{name} && find . -type f | sort | tr '\\n' ' '"), Duration::from_secs(10));
        sh(&mut s, "echo 'fn b() { todo!() }' > src/b.rs; echo new > c.txt; rm a.txt", Duration::from_secs(10));
        let changes = crate::share::pull(&case.dir).unwrap();
        let summary: Vec<String> = changes.iter().map(|c| format!("{:?}:{}", c.kind as u8, c.rel)).collect();
        eprintln!("--- shared {sent} files; inside: {inside}; changes: {summary:?}");
        for c in changes.iter().filter(|c| c.rel != "a.txt") {
            crate::share::apply(&case.dir, &case.log, c).unwrap();
        }
        let b = fs::read_to_string(host.join("src/b.rs")).unwrap();
        let c_txt = fs::read_to_string(host.join("c.txt")).unwrap_or_default();
        let a_kept = host.join("a.txt").exists();
        fs::remove_dir_all(&host).ok();
        let ver = sh(&mut s, "claude --version", Duration::from_secs(30));
        eprintln!("--- claude: {ver}");

        let log: Vec<String> = case.log.lock().unwrap().iter().map(|e| e.text.clone()).collect();
        eprintln!("--- activity: {log:#?}");
        case.delete();
        assert!(denied.contains("rc=") && !denied.contains("rc=0"), "example.com must be refused");
        assert!(npm.contains("1.3.0"), "npm reachable once allowed");
        assert!(api.contains("api=401"), "api reached with the placeholder swapped for the (fake) host key");
        assert!(sealed.contains("api=401"), "Claude still works while sealed: {sealed}");
        assert!(no_token.contains("api=401") && log.iter().any(|l| l.contains("not connected")), "no token: a clear 401, logged: {no_token}");
        assert!(env.lines().any(|l| l.trim().trim_start_matches("\u{1b}[?2004l").trim() == "0"), "the host key is not in the guest environment: {env}");
        assert_eq!(n, 1, "only the regular file comes out");
        assert!(exported, "the report reached the quarantine folder");
        assert!(!inside.contains("secret.env") && !inside.contains(".git/"), "ignored files and .git stay home: {inside}");
        assert!(inside.contains("./src/b.rs") && inside.contains("./a.txt"));
        assert_eq!(summary.len(), 3, "{summary:?}");
        assert!(b.contains("todo!()") && c_txt == "new\n", "approved changes applied");
        assert!(a_kept, "a denied deletion leaves the file alone");
    }
}

#[cfg(test)]
mod attach_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Types into a real `popcorn attach` session (under a pty via `script`) and times the echo.
    /// `cargo test --release -- --ignored attach_is_responsive --nocapture`
    #[test]
    #[ignore]
    fn attach_is_responsive() {
        unsafe {
            std::env::set_var("POPCORN_CLAUDE_KEY", "sk-ant-api03-fake-key-for-tests");
            std::env::set_var("POPCORN_USER", "tester");
        }
        let mut case = Case::create().unwrap();
        case.start().unwrap();
        let sock = case.dir.join("v.sock");
        assert!((0..80).any(|_| connect(&sock, SHELL_PORT).is_ok() || { std::thread::sleep(Duration::from_millis(250)); false }));
        let bin = concat!(env!("CARGO_MANIFEST_DIR"), "/target/release/popcorn");
        let cmd = format!("{bin} attach {}", case.dir.display());
        let mut child = Command::new("script").args(["-q", "-c", &cmd, "/dev/null"]).env("TERM", "xterm-256color").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut b = [0u8; 4096];
            while let Ok(n) = stdout.read(&mut b) {
                if n == 0 || tx.send(b[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let seen = std::cell::RefCell::new(String::new());
        // Wait until the output satisfies `done`, or the limit passes.
        let wait = |done: &dyn Fn(&str) -> bool, limit: Duration| -> Option<Duration> {
            let t0 = Instant::now();
            while t0.elapsed() < limit {
                if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(5)) {
                    seen.borrow_mut().push_str(&String::from_utf8_lossy(&chunk));
                }
                if done(&seen.borrow()) {
                    return Some(t0.elapsed());
                }
            }
            None
        };
        assert!(wait(&|s| s.contains("tester@popcorn"), Duration::from_secs(10)).is_some(), "prompt: {}", seen.borrow());
        std::thread::sleep(Duration::from_millis(400));
        while rx.try_recv().map(|c| seen.borrow_mut().push_str(&String::from_utf8_lossy(&c))).is_ok() {}
        // One key at a time, like typing; each must echo back.
        let mut worst = Duration::ZERO;
        for ch in "echo zq".chars() {
            let before = seen.borrow().len();
            stdin.write_all(ch.to_string().as_bytes()).unwrap();
            stdin.flush().unwrap();
            let took = wait(&|s| s.len() > before, Duration::from_secs(3)).unwrap_or(Duration::from_secs(3));
            worst = worst.max(took);
        }
        stdin.write_all(b"\r").unwrap();
        let out = wait(&|s| s.contains("\rzq\r\n"), Duration::from_secs(5));
        stdin.write_all(b"echo term=$TERM\r").unwrap();
        let term = wait(&|s| s.contains("term=xterm"), Duration::from_secs(3));
        eprintln!("worst keystroke echo: {worst:?}; command ran: {out:?}; TERM set: {term:?}");
        let tail: String = seen.borrow().chars().rev().take(200).collect::<String>().chars().rev().collect();
        eprintln!("--- transcript tail: {tail:?}");
        child.kill().ok();
        case.delete();
        assert!(out.is_some(), "command ran");
        assert!(worst < Duration::from_millis(150), "echo took {worst:?}");
    }
}
