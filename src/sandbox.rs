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

/// Where corns live: `home/corns`. Older versions used `home/cases`; move it on first use.
pub fn corns() -> PathBuf {
    let (old, new) = (home().join("cases"), home().join("corns"));
    if old.is_dir() && !new.exists() {
        fs::rename(&old, &new).ok();
    }
    new
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
        return Err("Start with a letter. Then use only letters, numbers and dashes.".into());
    }
    let p = std::env::home_dir().ok_or("no home folder")?.join(".config/popcorn/user");
    fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(p, &u).map_err(|e| e.to_string())?;
    Ok(u)
}

/// The app version. Each release has a matching workbench image.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// SHA-256 of the compressed image this release expects. Set at build time for releases;
/// unset in development builds, which build the image locally with Docker instead.
pub const IMAGE_SHA256: Option<&str> = option_env!("POPCORN_IMAGE_SHA256");

/// The image new corns use. Releases keep one file for each version, so an update never
/// changes the image under an existing corn.
pub fn golden() -> PathBuf {
    match IMAGE_SHA256 {
        Some(_) => home().join("images").join(format!("golden-{VERSION}.ext4")),
        None => home().join("golden.ext4"),
    }
}

/// Where Firecracker, the jailer and the guest kernel are: inside the AppImage, beside an
/// installed binary (`/usr/lib/popcorn`), or in the data folder for development.
pub fn vm_file(name: &str) -> PathBuf {
    let mut places = Vec::new();
    if let Some(dir) = std::env::var_os("APPDIR") {
        places.push(PathBuf::from(dir).join("usr/lib/popcorn"));
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("../lib/popcorn"))) {
        places.push(dir);
    }
    places.push(home());
    places.push(home().join("bin"));
    places.iter().map(|p| p.join(name)).find(|p| p.exists()).unwrap_or_else(|| home().join(name))
}

/// Download the release image, check it against the hash built into the app, then unpack it.
/// The check runs before any byte of the download is parsed. `got` counts downloaded bytes.
/// Fills `got` with bytes downloaded and `total` with the file size, when the server says it.
/// `got` becomes `u64::MAX` once the download is done and the check and unpack start.
pub fn download_image(got: &std::sync::atomic::AtomicU64, total: &std::sync::atomic::AtomicU64) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    let want = IMAGE_SHA256.ok_or("This build has no image to download.")?;
    let url = std::env::var("POPCORN_IMAGE_URL").unwrap_or_else(|_| format!("https://github.com/KunalSin9h/popcorn/releases/download/v{VERSION}/golden-{VERSION}.ext4.zst"));
    let dir = home().join("images");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let zst = dir.join(format!("golden-{VERSION}.ext4.zst.part"));
    // The size, from the last response after redirects.
    if let Ok(o) = Command::new("curl").args(["-fsSLI"]).arg(&url).output() {
        let len = String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse().ok())).last();
        total.store(len.unwrap_or(0), Ordering::Relaxed);
    }
    let mut curl = Command::new("curl").args(["-fsSL", "--retry", "3", "-o"]).arg(&zst).arg(&url).spawn().map_err(|e| format!("Popcorn needs curl to download the image: {e}"))?;
    let ok = loop {
        if let Some(st) = curl.try_wait().map_err(|e| e.to_string())? {
            break st.success();
        }
        got.store(fs::metadata(&zst).map(|m| m.len()).unwrap_or(0), Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(250));
    };
    if !ok {
        fs::remove_file(&zst).ok();
        return Err("The download failed. Check your connection, then try again.".into());
    }
    got.store(u64::MAX, Ordering::Relaxed);
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut f = fs::File::open(&zst).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    let have: String = ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect();
    if have != want {
        fs::remove_file(&zst).ok();
        return Err("The downloaded image does not match this release. Popcorn deleted it.".into());
    }
    let part = dir.join(format!("golden-{VERSION}.ext4.part"));
    let mut dec = ruzstd::decoding::StreamingDecoder::new(fs::File::open(&zst).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let mut out = fs::File::create(&part).map_err(|e| e.to_string())?;
    std::io::copy(&mut dec, &mut out).map_err(|e| format!("Could not unpack the image: {e}"))?;
    fs::remove_file(&zst).ok();
    fs::rename(&part, golden()).map_err(|e| e.to_string())
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
        let mut out: Vec<Case> = fs::read_dir(corns())
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
        let policy = Policy::new(mode);
        Case { name, dir, mode, policy, log, started: None, vm: None }
    }

    /// A new case: a fresh, empty, sparse disk. Creating one takes well under a second.
    pub fn create() -> Result<Case, String> {
        let root = corns();
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
        // Remember the image this corn starts from, so later updates leave it alone.
        fs::write(dir.join("image"), golden().to_string_lossy().as_bytes()).ok();
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


    pub fn start(&mut self) -> Result<(), String> {
        if self.running() {
            return Ok(());
        }
        // Corns made before images had versions use the old single image file.
        let image = fs::read_to_string(self.dir.join("image")).map(|p| PathBuf::from(p.trim())).unwrap_or_else(|_| home().join("golden.ext4"));
        if !image.exists() {
            return Err(format!("This corn needs the image {}. It is not on this computer.", image.display()));
        }
        // No token yet is fine: the corn runs, and Claude inside gets the API's authentication error.
        let auth = gate::credential().map(|c| c.kind).unwrap_or(gate::Auth::Key);
        fs::create_dir_all(self.dir.join("vm")).map_err(|e| e.to_string())?;
        for p in [sock(&self.dir), gate::port_path(&sock(&self.dir), 1080), gate::port_path(&sock(&self.dir), 1081)] {
            fs::remove_file(p).ok();
        }
        // The gate must be listening before the guest dials out.
        gate::serve(&sock(&self.dir), self.policy.clone(), self.log.clone(), self.dir.clone())?;
        // Skills synced from your computer (Settings), as a third, read-only drive.
        let skills = Some(crate::skills::image()).filter(|p| p.exists());
        let config = format!(
            r#"{{
  "boot-source": {{"kernel_image_path": "{kernel}", "boot_args": "console=ttyS0 reboot=k panic=1 pci=off quiet init=/sbin/popcorn-init popcorn.auth={auth} popcorn.user={user}"}},
  "drives": [
    {{"drive_id": "golden", "path_on_host": "{golden}", "is_root_device": true, "is_read_only": true}},
    {{"drive_id": "case", "path_on_host": "{disk}", "is_root_device": false, "is_read_only": false}}{skills_drive}
  ],
  "machine-config": {{"vcpu_count": 2, "mem_size_mib": 2048}},
  "vsock": {{"guest_cid": 3, "uds_path": "{sock}"}}
}}"#,
            kernel = vm_file("vmlinux").display(),
            auth = auth.as_str(),
            user = user_name().unwrap_or_else(|| "analyst".into()),
            golden = image.display(),
            disk = self.dir.join("disk.ext4").display(),
            sock = sock(&self.dir).display(),
            skills_drive = match &skills {
                Some(p) => format!(",\n    {{\"drive_id\": \"skills\", \"path_on_host\": \"{}\", \"is_root_device\": false, \"is_read_only\": true}}", p.display()),
                None => String::new(),
            },
        );
        fs::write(self.dir.join("vm.json"), config).map_err(|e| e.to_string())?;
        let console = fs::File::create(self.dir.join("console.log")).map_err(|e| e.to_string())?;
        // The second wall: Firecracker runs inside bubblewrap, with its own empty view of the
        // system. It sees only its own binary, the kernel and the image (read-only), this corn's
        // disk and its `vm/` folder, and /dev/kvm. No network, no home folder, no other processes,
        // no capabilities, and no new user namespaces (a common first step of kernel exploits).
        // Firecracker's own seccomp filters stay on.
        // ponytail: runs as your user; `jailer` (needs root) would add a separate user and cgroups.
        let (firecracker, kernel, vm_json, disk) = (vm_file("firecracker"), vm_file("vmlinux"), self.dir.join("vm.json"), self.dir.join("disk.ext4"));
        let mut cmd = Command::new("bwrap");
        cmd.args(["--unshare-all", "--unshare-user", "--disable-userns", "--die-with-parent", "--new-session", "--cap-drop", "ALL", "--clearenv"]);
        for ro in [&firecracker, &kernel, &image, &vm_json].into_iter().chain(skills.as_ref()) {
            cmd.arg("--ro-bind").arg(ro).arg(ro);
        }
        for rw in [&disk, &self.dir.join("vm")] {
            cmd.arg("--bind").arg(rw).arg(rw);
        }
        // The third wall: `popcorn landlock` starts Firecracker under Landlock, limited to exactly
        // these files. It needs the system libraries to start; Landlock then closes them too.
        let me = me()?;
        cmd.arg("--ro-bind").arg(&me).arg(&me);
        cmd.args(["--ro-bind", "/usr", "/usr", "--symlink", "usr/lib", "/lib", "--symlink", "usr/lib64", "/lib64"]);
        cmd.args(["--dev-bind", "/dev/kvm", "/dev/kvm", "--proc", "/proc", "--"]).arg(&me).arg("landlock");
        cmd.arg(format!("x:{}", firecracker.display()));
        for ro in [&kernel, &image, &vm_json].into_iter().chain(skills.as_ref()) {
            cmd.arg(format!("ro:{}", ro.display()));
        }
        cmd.arg(format!("rw:{}", disk.display()))
            .arg(format!("sock:{}", self.dir.join("vm").display()))
            .args(["dev:/dev/kvm", "--"])
            .arg(&firecracker)
            .args(["--no-api", "--config-file"])
            .arg(&vm_json)
            .stdin(Stdio::null())
            .stdout(console.try_clone().map_err(|e| e.to_string())?)
            .stderr(console);
        // The VM dies with Popcorn, even on a crash or a kill. Otherwise it keeps running unseen,
        // and a second start would boot a second VM on the same disk.
        unsafe {
            unsafe extern "C" {
                fn prctl(option: i32, ...) -> i32;
            }
            const PR_SET_PDEATHSIG: i32 = 1;
            const SIGKILL: u64 = 9;
            std::os::unix::process::CommandExt::pre_exec(&mut cmd, || {
                prctl(PR_SET_PDEATHSIG, SIGKILL);
                Ok(())
            });
        }
        let child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "Popcorn needs bubblewrap to run a corn. Install the bubblewrap package.".to_string(),
            _ => format!("Popcorn cannot start the corn: {e}"),
        })?;
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

    /// True while a terminal window is attached: each `attach` holds a shared lock on this file.
    pub fn terminal_open(&self) -> bool {
        fs::File::open(self.dir.join("terminal.lock")).is_ok_and(|f| f.try_lock().is_err())
    }

    /// Open a terminal window with a shell inside the case.
    pub fn open_terminal(&self) -> Result<(), String> {
        open_in_terminal(&[me()?.as_os_str(), "attach".as_ref(), self.dir.as_os_str()])
    }
}

/// This program. After an upgrade or rebuild replaces the running binary, Linux reports its path
/// with " (deleted)". The new binary at the same path works the same way, so use it.
fn me() -> Result<PathBuf, String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    // Tests run as a test binary; the VM tests need the real program, as the attach test does.
    #[cfg(test)]
    let me = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/target/release/popcorn"));
    Ok(match me.to_string_lossy().strip_suffix(" (deleted)") {
        Some(path) => PathBuf::from(path),
        None => me,
    })
}

/// The corn's vsock socket. It lives in `vm/`, the only folder of the corn that Firecracker can
/// write to, so a compromised Firecracker cannot change the corn's other files (such as `share`).
pub fn sock(dir: &Path) -> PathBuf {
    dir.join("vm/v.sock")
}

/// `popcorn landlock <rule>... -- <command>`: the third wall. It runs inside bubblewrap, just
/// before Firecracker. Landlock, a kernel feature that needs no root, limits this process and all
/// it starts to the files named here, so code that gets out of bubblewrap's view still cannot
/// open anything else. Rules: `x:` run a file, `ro:` read a file, `rw:` read and write a file,
/// `dev:` use a device, `sock:` make sockets in a folder. Where the kernel supports it, TCP is
/// closed, abstract sockets outside are unreachable, and signals cannot leave.
pub fn landlock_exec(args: &[String]) -> ! {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    use std::os::unix::process::CommandExt;
    let fail = |m: String| -> ! {
        eprintln!("popcorn landlock: {m}");
        std::process::exit(126)
    };
    unsafe extern "C" {
        fn syscall(n: i64, ...) -> i64;
        fn prctl(option: i32, ...) -> i32;
    }
    const CREATE_RULESET: i64 = 444;
    const ADD_RULE: i64 = 445;
    const RESTRICT_SELF: i64 = 446;
    const EXECUTE: u64 = 1;
    const WRITE_FILE: u64 = 1 << 1;
    const READ_FILE: u64 = 1 << 2;
    const READ_DIR: u64 = 1 << 3;
    const MAKE_SOCK: u64 = 1 << 9;
    const IOCTL_DEV: u64 = 1 << 15;
    #[repr(C)]
    struct Ruleset {
        fs: u64,
        net: u64,
        scoped: u64,
    }
    #[repr(C, packed)]
    struct PathBeneath {
        allowed: u64,
        parent_fd: i32,
    }
    let Some(split) = args.iter().position(|a| a == "--") else { fail("no command".into()) };
    let (rules, cmd) = (&args[..split], &args[split + 1..]);
    if cmd.is_empty() {
        fail("no command".into());
    }
    let abi = unsafe { syscall(CREATE_RULESET, std::ptr::null::<Ruleset>(), 0usize, 1u32) };
    if abi < 1 {
        // ponytail: kernels without Landlock run with two walls; make this fatal once all supported distros have it.
        eprintln!("popcorn landlock: this kernel has no Landlock; Firecracker runs inside bubblewrap only");
        let e = Command::new(&cmd[0]).args(&cmd[1..]).exec();
        fail(format!("cannot start {}: {e}", cmd[0]));
    }
    // Every right this kernel knows is handled, so anything not granted below is denied.
    let mut fs_all = (1 << 13) - 1;
    for (min, bit) in [(2, 1 << 13), (3, 1 << 14), (5, IOCTL_DEV)] {
        if abi >= min {
            fs_all |= bit;
        }
    }
    let attr = Ruleset { fs: fs_all, net: if abi >= 4 { 3 } else { 0 }, scoped: if abi >= 6 { 3 } else { 0 } };
    let size: usize = if abi >= 6 { 24 } else if abi >= 4 { 16 } else { 8 };
    let ruleset = unsafe { syscall(CREATE_RULESET, &attr as *const Ruleset, size, 0u32) };
    if ruleset < 0 {
        fail(format!("cannot create a ruleset: {}", std::io::Error::last_os_error()));
    }
    for rule in rules {
        let (kind, path) = rule.split_once(':').unwrap_or_else(|| fail(format!("bad rule {rule}")));
        let allowed = match kind {
            "x" => EXECUTE | READ_FILE,
            "ro" => READ_FILE,
            "rw" => READ_FILE | WRITE_FILE,
            "dev" => READ_FILE | WRITE_FILE | if abi >= 5 { IOCTL_DEV } else { 0 },
            "sock" => READ_DIR | MAKE_SOCK,
            _ => fail(format!("bad rule {rule}")),
        };
        const O_PATH: i32 = 0o10000000;
        let f = fs::OpenOptions::new().read(true).custom_flags(O_PATH).open(path).unwrap_or_else(|e| fail(format!("{path}: {e}")));
        let beneath = PathBeneath { allowed, parent_fd: f.as_raw_fd() };
        if unsafe { syscall(ADD_RULE, ruleset, 1i32, &beneath as *const PathBeneath, 0u32) } < 0 {
            fail(format!("cannot add {rule}: {}", std::io::Error::last_os_error()));
        }
    }
    const PR_SET_NO_NEW_PRIVS: i32 = 38;
    if unsafe { prctl(PR_SET_NO_NEW_PRIVS, 1u64, 0u64, 0u64, 0u64) } != 0 || unsafe { syscall(RESTRICT_SELF, ruleset, 0u32) } != 0 {
        fail(format!("cannot restrict: {}", std::io::Error::last_os_error()));
    }
    let e = Command::new(&cmd[0]).args(&cmd[1..]).exec();
    fail(format!("cannot start {}: {e}", cmd[0]))
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
    let (term, args) = TERMINALS.iter().find(|(t, _)| std::env::split_paths(&path).any(|d| d.join(t).exists())).ok_or("Popcorn found no terminal app.")?;
    Command::new(term).args(*args).args(cmd).spawn().map_err(|e| format!("Popcorn cannot open {term}: {e}"))?;
    Ok(())
}

/// Copy a file or folder from the host into ~/inbox in a running case, as a tar stream over vsock.
/// A name already sent to this corn's inbox gets a number, so a copy never replaces a file.
pub fn send(dir: &Path, log: &Log, path: &Path) -> Result<(), String> {
    crate::share::check_source(path)?;
    let (parent, name) = (path.parent().ok_or("bad path")?, path.file_name().ok_or("bad path")?);
    let name_s = name.to_string_lossy().into_owned();
    let sent_file = dir.join("inbox.sent");
    let sent = fs::read_to_string(&sent_file).unwrap_or_default();
    let taken = |n: &str| sent.lines().any(|l| l == n);
    let as_name = if taken(&name_s) { (2..).map(|i| numbered(&name_s, i)).find(|n| !taken(n)).unwrap() } else { name_s.clone() };
    let mut tar = Command::new("tar");
    tar.arg("-c").arg("-C").arg(parent);
    if as_name != name_s {
        // Rename the top entry in the stream: s,^OLD,NEW, with regex and separator characters escaped.
        let esc = |s: &str, special: &str| s.chars().flat_map(|c| if special.contains(c) { vec!['\\', c] } else { vec![c] }).collect::<String>();
        tar.arg(format!("--transform=s,^{},{},S", esc(&name_s, "\\.[]*^$,"), esc(&as_name, "\\&,")));
    }
    let mut tar = tar.arg(name).stdout(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let mut s = connect(&sock(dir), FILES_PORT)?;
    std::io::copy(tar.stdout.as_mut().unwrap(), &mut s).map_err(|e| e.to_string())?;
    s.shutdown(std::net::Shutdown::Write).ok();
    let mut sink = Vec::new();
    s.read_to_end(&mut sink).ok();
    if !tar.wait().map(|s| s.success()).unwrap_or(false) {
        return Err(format!("Popcorn cannot pack {name_s}."));
    }
    fs::OpenOptions::new().create(true).append(true).open(&sent_file).and_then(|mut f| writeln!(f, "{as_name}")).ok();
    gate::record(log, dir, Event::note(&format!("Copied {as_name} into ~/inbox")));
    Ok(())
}

/// `report.pdf` -> `report (2).pdf`; `pkg` -> `pkg (2)`.
fn numbered(name: &str, i: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({i}).{ext}"),
        _ => format!("{name} ({i})"),
    }
}

/// Pull ~/outbox from a running case into `<case>/exported/<n>`, a quarantine folder on the host.
pub fn export(dir: &Path, log: &Log) -> Result<(PathBuf, usize), String> {
    // Keep our side open: socat ends the guest command shortly after it sees our EOF.
    let mut s = connect(&sock(dir), OUTBOX_PORT)?;
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

/// Drops clipboard escapes from the corn's output: OSC 52 and kitty's OSC 5522. Through them,
/// code in a corn could set your clipboard (a command you later paste on your computer) or read
/// it. All other output passes unchanged. A sequence can arrive in pieces, so state stays between
/// reads. Both the `ESC ]` and the UTF-8 C1 form start a sequence; BEL, ST, CAN or SUB end it.
// ponytail: blocks the clipboard sequences only; other OSCs (title, links, colors) pass.
#[derive(Default)]
pub struct NoClipboard {
    state: Clip,
    held: Vec<u8>,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Clip {
    #[default]
    Text,
    Esc,
    C2,
    Number,
    Drop,
    DropEsc,
    DropC2,
}

impl NoClipboard {
    pub fn filter(&mut self, input: &[u8], out: &mut Vec<u8>) {
        for &b in input {
            self.byte(b, out);
        }
    }

    fn byte(&mut self, b: u8, out: &mut Vec<u8>) {
        match self.state {
            Clip::Text if b == 0x1b || b == 0xc2 => {
                self.held.push(b);
                self.state = if b == 0x1b { Clip::Esc } else { Clip::C2 };
            }
            Clip::Text => out.push(b),
            Clip::Esc | Clip::C2 if (self.state, b) == (Clip::Esc, b']') || (self.state, b) == (Clip::C2, 0x9d) => {
                self.held.push(b);
                self.state = Clip::Number;
            }
            Clip::Esc | Clip::C2 => {
                out.append(&mut self.held);
                self.state = Clip::Text;
                self.byte(b, out);
            }
            // Leading zeros count: terminals read "052" as 52. A very long number is dropped too.
            Clip::Number if b.is_ascii_digit() && self.held.len() < 40 => self.held.push(b),
            Clip::Number => {
                let n = std::str::from_utf8(&self.held[2..]).unwrap_or("").trim_start_matches('0');
                if n == "52" || n == "5522" || b.is_ascii_digit() {
                    self.held.clear();
                    self.state = Clip::Drop;
                } else {
                    out.append(&mut self.held);
                    self.state = Clip::Text;
                }
                self.byte(b, out);
            }
            Clip::Drop => match b {
                0x07 | 0x18 | 0x1a => self.state = Clip::Text,
                0x1b => self.state = Clip::DropEsc,
                0xc2 => self.state = Clip::DropC2,
                _ => {}
            },
            // ESC \ is ST and ends the sequence. Any other ESC also ends it and starts a new escape.
            Clip::DropEsc if b == b'\\' => self.state = Clip::Text,
            Clip::DropEsc => {
                self.held.push(0x1b);
                self.state = Clip::Esc;
                self.byte(b, out);
            }
            Clip::DropC2 if b == 0x9c => self.state = Clip::Text,
            Clip::DropC2 => {
                self.state = Clip::Drop;
                self.byte(b, out);
            }
        }
    }
}

/// `popcorn attach <case dir>`: bridge this terminal to the shell inside the case.
pub fn attach(dir: &Path) -> ! {
    let sock = sock(dir);
    let mut s = match (1..=40).find_map(|_| connect(&sock, SHELL_PORT).ok().or_else(|| {
        std::thread::sleep(std::time::Duration::from_millis(250));
        None
    })) {
        Some(s) => s,
        None => {
            eprintln!("Popcorn cannot connect to the corn. Make sure that it runs.");
            std::thread::sleep(std::time::Duration::from_secs(4));
            std::process::exit(1);
        }
    };
    // Hold this lock until the terminal closes, so Popcorn knows a terminal is open.
    let _terminal = fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("terminal.lock")).and_then(|f| f.lock_shared().map(|_| f));
    // Tell the guest shell what terminal it is talking to and how big it is, then clear the
    // screen. The leading space keeps this line out of the shell history.
    let term = std::env::var("TERM").ok().filter(|t| !t.is_empty() && t != "dumb").unwrap_or_else(|| "xterm-256color".into());
    let size = Command::new("stty").arg("size").stdin(Stdio::inherit()).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    // Make this window unmistakable: title, background, cursor (standard escape codes; a terminal
    // that doesn't know one ignores it), and a prompt badge that works everywhere.
    let name: String = case_name(dir).chars().filter(|c| c.is_alphanumeric() || " ._-".contains(*c)).collect();
    {
        let mut o = std::io::stdout();
        // "Husk": warm charcoal, off-white text, corn-yellow cursor. The 16 ANSI colors keep their
        // usual meaning and all read at 5:1 or more on the charcoal, with one change: Claude Code
        // runs with an ANSI theme in the corn and draws its mark in bright red (slot 9), so slot 9 is green.
        const PALETTE: [&str; 16] = [
            "#2e2c29", "#f07a62", "#8fd18a", "#f0be2c", "#7db8f0", "#e39bd0", "#5fd6c4", "#d6d2ca",
            "#9a958c", "#5ee08a", "#a8e6a0", "#ffd86a", "#a2cdf7", "#f0b8e2", "#8be6d8", "#f7f5f0",
        ];
        let palette: String = PALETTE.iter().enumerate().map(|(i, c)| format!("\x1b]4;{i};{c}\x07")).collect();
        let _ = write!(o, "\x1b]0;🌽 {name}\x07\x1b]10;#ebe8e2\x07\x1b]11;#1c1b19\x07\x1b]12;#f0be2c\x07\x1b]17;#3d3a35\x07{palette}");
        let _ = o.flush();
    }
    let ps1 = format!(r"\[\e[1;30;43m\] {name} \[\e[0m\] \[\e[32m\]\u@popcorn\[\e[0m\]:\[\e[36m\]\w\[\e[0m\]\$ ");
    // Pass on 24-bit color, so tools with their own colors (Claude Code's orange) keep them.
    let colorterm = std::env::var("COLORTERM").ok().filter(|c| c == "truecolor" || c == "24bit").unwrap_or_else(|| "truecolor".into());
    let mut setup = format!(" export TERM={term} COLORTERM={colorterm} PS1='{ps1}'");
    if let Some((rows, cols)) = size.as_deref().and_then(|s| s.split_once(' ')).filter(|(r, c)| *r != "0" && *c != "0") {
        // ponytail: size is set once; resizing the window later does not reach the guest.
        setup.push_str(&format!("; stty rows {rows} cols {cols}"));
    }
    // Claude Code in an ANSI theme takes its colors from this terminal, so its mark turns green.
    // Only the image's default ("dark") is switched; a theme you picked stays.
    // ponytail: edits ~/.claude.json on each attach; move to the image's claude.json at the next release.
    setup.push_str(r#"; sed -i 's/"theme": *"dark"/"theme": "dark-ansi"/' ~/.claude.json 2>/dev/null"#);
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
    let (mut guard, mut clean) = (NoClipboard::default(), Vec::with_capacity(8192));
    while let Ok(n) = s.read(&mut buf) {
        clean.clear();
        guard.filter(&buf[..n], &mut clean);
        if n == 0 || out.write_all(&clean).and_then(|_| out.flush()).is_err() {
            break;
        }
    }
    // Give the terminal its own colors back.
    let _ = out.write_all(b"\x1b]104\x07\x1b]110\x07\x1b]111\x07\x1b]112\x07\x1b]117\x07");
    let _ = out.flush();
    drop(out);
    Command::new("stty").arg("sane").stdin(Stdio::inherit()).status().ok();
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_get_numbers() {
        assert_eq!(numbered("report.pdf", 2), "report (2).pdf");
        assert_eq!(numbered("pkg", 3), "pkg (3)");
        assert_eq!(numbered(".env", 2), ".env (2)");
    }

    #[test]
    fn terminal_drops_clipboard_escapes() {
        let run = |chunks: &[&[u8]]| {
            let (mut g, mut out) = (NoClipboard::default(), Vec::new());
            chunks.iter().for_each(|c| g.filter(c, &mut out));
            String::from_utf8_lossy(&out).into_owned()
        };
        let whole = |s: &str| run(&[s.as_bytes()]);
        // Clipboard writes and reads, every form, are dropped; the text around them stays.
        assert_eq!(whole("a\x1b]52;c;Y3VybCBldmlsIHwgc2g=\x07b"), "ab");
        assert_eq!(whole("a\x1b]52;c;?\x1b\\b"), "ab");
        assert_eq!(whole("a\u{9d}52;c;eA==\u{9c}b"), "ab");
        assert_eq!(whole("a\x1b]052;c;eA==\x07b"), "ab");
        assert_eq!(whole("a\x1b]5522;type=write\x07b"), "ab");
        // An ESC cuts the sequence short; the escape it starts passes.
        assert_eq!(whole("a\x1b]52;c;eA==\x1b[31mb"), "a\x1b[31mb");
        // A sequence split into single bytes is dropped too.
        let bytes = "a\x1b]52;c;eA==\x07b".as_bytes();
        assert_eq!(run(&bytes.chunks(1).collect::<Vec<_>>()), "ab");
        // Everything else passes unchanged: colors, titles, links, UTF-8 (© is C2 A9).
        for ok in ["\x1b[1;32mgreen\x1b[0m", "\x1b]0;title\x07", "\x1b]8;;https://x.y\x1b\\link\x1b]8;;\x1b\\", "\x1b]11;?\x07", "© ±", "\x1b]5;x\x07"] {
            assert_eq!(whole(ok), ok);
        }
    }

    #[test]
    fn terminal_lock_shows_an_open_terminal() {
        let dir = std::env::temp_dir().join(format!("popcorn-lock-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let case = Case::open(dir.clone());
        assert!(!case.terminal_open(), "no lock file: no terminal");
        let held = fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("terminal.lock")).unwrap();
        held.lock_shared().unwrap();
        assert!(case.terminal_open(), "an attach holds the lock");
        drop(held);
        assert!(!case.terminal_open(), "the terminal closed");
        fs::remove_dir_all(dir).ok();
    }

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
        // A synced skill: a read-only drive that the corn copies into ~/.claude/skills at boot.
        let skill_src = std::env::temp_dir().join(format!("popcorn-skill-src-{}", std::process::id()));
        fs::create_dir_all(skill_src.join("demo")).unwrap();
        fs::write(skill_src.join("demo/SKILL.md"), "# demo skill").unwrap();
        crate::skills::pack(&skill_src, &["demo".to_string()].into(), &home()).unwrap();
        fs::remove_dir_all(&skill_src).ok();
        let mut case = Case::create().unwrap();
        case.start().unwrap();
        let sock = sock(&case.dir);
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
                    let ok = a.target.starts_with("registry.npmjs.org") || a.target.starts_with("localtest.me");
                    a.answer(if ok { gate::Answer::Once } else { gate::Answer::Deny });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let denied = sh(&mut s, "curl -sS -m 20 https://example.com -o /dev/null; echo rc=$?", Duration::from_secs(30));
        // Host-internal addresses stay closed: a literal one without asking, an allowed name that
        // resolves to one after the check.
        sh(&mut s, "curl -sS -m 10 https://169.254.169.254/ -o /dev/null; curl -sS -m 10 https://localtest.me/ -o /dev/null", Duration::from_secs(30));
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
        let inside = sh(&mut s, &format!("cd ~/work/{name} && find . -path ./.git -prune -o -type f -print | sort | tr '\\n' ' '; grep -c token .git/config"), Duration::from_secs(10));
        sh(&mut s, "echo 'fn b() { todo!() }' > src/b.rs; echo new > c.txt; rm a.txt", Duration::from_secs(10));
        let changes = crate::share::pull(&case.dir, &case.log).unwrap();
        let summary: Vec<String> = changes.iter().map(|c| format!("{:?}:{}", c.kind as u8, c.rel)).collect();
        eprintln!("--- shared {sent} files; inside: {inside}; changes: {summary:?}");
        for c in changes.iter().filter(|c| c.rel != "a.txt") {
            crate::share::apply(&case.dir, &case.log, c).unwrap();
        }
        let b = fs::read_to_string(host.join("src/b.rs")).unwrap();
        let c_txt = fs::read_to_string(host.join("c.txt")).unwrap_or_default();
        let a_kept = host.join("a.txt").exists();
        // Copy in the same file twice: the second gets a number and the first stays.
        fs::write(host.join("a.b [x].txt"), "first").unwrap();
        send(&case.dir, &case.log, &host.join("a.b [x].txt")).unwrap();
        fs::write(host.join("a.b [x].txt"), "second").unwrap();
        send(&case.dir, &case.log, &host.join("a.b [x].txt")).unwrap();
        let inbox = sh(&mut s, "cat ~/inbox/'a.b [x].txt'; echo; cat ~/inbox/'a.b [x] (2).txt'", Duration::from_secs(10));
        // Stop sharing: Popcorn forgets the folder.
        crate::share::stop(&case.dir, &case.log);
        let unshared = crate::share::get(&case.dir).is_none();
        fs::remove_dir_all(&host).ok();
        let ping = sh(&mut s, "ping -c1 -W2 127.0.0.1 | grep -c ' 0% packet loss'", Duration::from_secs(10));
        let skill = sh(&mut s, "cat ~/.claude/skills/demo/SKILL.md; stat -c 'owner=%U' ~/.claude/skills/demo/SKILL.md; grep -c vdc /proc/mounts", Duration::from_secs(10));
        let ver = sh(&mut s, "claude --version", Duration::from_secs(30));
        eprintln!("--- claude: {ver}");

        let log: Vec<String> = case.log.lock().unwrap().iter().map(|e| e.text.clone()).collect();
        eprintln!("--- activity: {log:#?}");
        case.delete();
        assert!(denied.contains("rc=") && !denied.contains("rc=0"), "example.com must be refused");
        assert!(log.iter().any(|l| l == "169.254.169.254:443 (address on your network)"), "metadata address refused: {log:?}");
        assert!(log.iter().any(|l| l == "localtest.me:443 (resolves to an address on your network)"), "allowed name with an internal address refused");
        assert!(npm.contains("1.3.0"), "npm reachable once allowed");
        assert!(api.contains("api=401"), "api reached with the placeholder swapped for the (fake) host key");
        assert!(sealed.contains("api=401"), "Claude still works while sealed: {sealed}");
        assert!(no_token.contains("api=401"), "no token: the API's own 401 reaches the corn: {no_token}");
        assert!(env.lines().any(|l| l.trim().trim_start_matches("\u{1b}[?2004l").trim() == "0"), "the host key is not in the guest environment: {env}");
        assert_eq!(n, 1, "only the regular file comes out");
        assert!(exported, "the report reached the quarantine folder");
        assert!(ping.lines().any(|l| l.trim().trim_start_matches("\u{1b}[?2004l").trim() == "1"), "ping works for the user: {ping}");
        assert!(!inside.contains("secret.env") && !inside.contains("./.git/") && !inside.contains("\n1"), "ignored files and your .git stay home: {inside}");
        assert!(inside.contains("./src/b.rs") && inside.contains("./a.txt"));
        assert_eq!(summary.len(), 3, "{summary:?}");
        assert!(b.contains("todo!()") && c_txt == "new\n", "approved changes applied");
        assert!(a_kept, "a denied deletion leaves the file alone");
        assert!(inbox.contains("first") && inbox.contains("second"), "a copy never replaces a file: {inbox}");
        assert!(unshared, "stop sharing forgets the folder");
        assert!(skill.contains("# demo skill") && skill.contains("owner=tester"), "the synced skill is in the corn: {skill}");
        assert!(skill.lines().any(|l| l.trim().trim_start_matches("\u{1b}[?2004l").trim() == "0"), "the skills drive is not left mounted: {skill}");
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
        let sock = sock(&case.dir);
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

#[cfg(test)]
mod download_tests {
    /// Needs dist/ from scripts/release-image.sh and a build with POPCORN_IMAGE_SHA256 set:
    /// POPCORN_IMAGE_SHA256=$(cut -d' ' -f1 dist/*.sha256) cargo test --release -- --ignored download
    #[test]
    #[ignore]
    fn download_checks_the_hash_then_unpacks() {
        let dist = concat!(env!("CARGO_MANIFEST_DIR"), "/dist");
        let home = std::env::temp_dir().join(format!("popcorn-dl-{}", std::process::id()));
        unsafe {
            std::env::set_var("POPCORN_HOME", &home);
            std::env::set_var("POPCORN_IMAGE_URL", format!("file://{dist}/golden-{}.ext4.zst", super::VERSION));
        }
        let (got, total) = (std::sync::atomic::AtomicU64::new(0), std::sync::atomic::AtomicU64::new(0));
        super::download_image(&got, &total).unwrap();
        assert!(total.load(std::sync::atomic::Ordering::Relaxed) > 0, "size known before download");
        let size = std::fs::metadata(super::golden()).unwrap().len();
        assert!(size > 1 << 30, "unpacked image is {size} bytes");

        // A file that does not match the built-in hash is refused and deleted.
        let bad = home.join("bad.zst");
        std::fs::write(&bad, b"not the image").unwrap();
        std::fs::remove_file(super::golden()).unwrap();
        unsafe { std::env::set_var("POPCORN_IMAGE_URL", format!("file://{}", bad.display())) };
        let err = super::download_image(&got, &total).unwrap_err();
        assert!(err.contains("does not match"), "{err}");
        assert!(!super::golden().exists());
        std::fs::remove_dir_all(&home).ok();
    }
}
