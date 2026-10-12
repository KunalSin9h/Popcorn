//! The only way out of a case. The guest dials two vsock ports, which Firecracker turns into
//! unix sockets here: 1080 is an HTTP CONNECT proxy that asks you about every new host, 1081 is
//! the Claude API, where the real credential is added on the host so it never exists in the VM.
use crate::sandbox::Mode;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::collections::BTreeSet;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Unanswered requests are denied after this long.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Clone, Copy, PartialEq)]
pub enum Answer {
    Once,
    Always,
    Deny,
}

/// A connection held open while you decide.
pub struct Ask {
    pub id: u64,
    pub target: String,
    pub at: u64,
    reply: Sender<Answer>,
}

impl Ask {
    pub fn answer(self, a: Answer) {
        let _ = self.reply.send(a);
    }
}

/// What a case may reach. Shared by the gate threads and the UI.
pub struct Policy {
    pub mode: Mutex<Mode>,
    /// Hosts you chose "Always allow" for, saved in the case's `allow` file.
    pub allow: Mutex<BTreeSet<String>>,
    pub asks: Mutex<Vec<Ask>>,
    dir: PathBuf,
}

impl Policy {
    pub fn load(dir: &Path, mode: Mode) -> Arc<Policy> {
        let allow = fs::read_to_string(dir.join("allow")).unwrap_or_default().lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        Arc::new(Policy { mode: Mutex::new(mode), allow: Mutex::new(allow), asks: Mutex::new(Vec::new()), dir: dir.to_path_buf() })
    }

    pub fn forget(&self, host: &str) {
        let mut a = self.allow.lock().unwrap();
        a.remove(host);
        fs::write(self.dir.join("allow"), a.iter().cloned().collect::<Vec<_>>().join("\n")).ok();
    }

    fn remember(&self, host: &str) {
        let mut a = self.allow.lock().unwrap();
        a.insert(host.to_string());
        fs::write(self.dir.join("allow"), a.iter().cloned().collect::<Vec<_>>().join("\n")).ok();
    }

    /// Decide about one connection. May block for up to ASK_TIMEOUT while you answer.
    fn decide(&self, target: &str) -> (bool, &'static str) {
        if *self.mode.lock().unwrap() == Mode::Sealed {
            return (false, "sealed");
        }
        if self.allow.lock().unwrap().contains(target) {
            return (true, "");
        }
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let (tx, rx) = channel();
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.asks.lock().unwrap().push(Ask { id, target: target.to_string(), at: now(), reply: tx });
        let answer = rx.recv_timeout(ASK_TIMEOUT).unwrap_or(Answer::Deny);
        self.asks.lock().unwrap().retain(|a| a.id != id);
        match answer {
            Answer::Once => (true, "allowed once"),
            Answer::Always => {
                self.remember(target);
                (true, "always allowed")
            }
            Answer::Deny => (false, "denied by you"),
        }
    }
}
const API: &str = "api.anthropic.com";

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Allowed,
    Blocked,
    Claude,
    Note,
}

#[derive(Clone)]
pub struct Event {
    pub at: u64,
    pub kind: Kind,
    pub text: String,
}

impl Event {
    pub fn note(text: &str) -> Event {
        Event { at: now(), kind: Kind::Note, text: text.into() }
    }
}

pub type Log = Arc<Mutex<Vec<Event>>>;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Append to the case's activity, in memory and in `activity.log`.
pub fn record(log: &Log, dir: &Path, e: Event) {
    let tag = match e.kind {
        Kind::Allowed => "allowed",
        Kind::Blocked => "blocked",
        Kind::Claude => "claude",
        Kind::Note => "note",
    };
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(dir.join("activity.log")) {
        let _ = writeln!(f, "{}\t{tag}\t{}", e.at, e.text.replace(['\n', '\t'], " "));
    }
    log.lock().unwrap().push(e);
}

pub fn read_log(dir: &Path) -> Vec<Event> {
    fs::read_to_string(dir.join("activity.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut p = l.splitn(3, '\t');
            let at = p.next()?.parse().ok()?;
            let kind = match p.next()? {
                "allowed" => Kind::Allowed,
                "blocked" => Kind::Blocked,
                "claude" => Kind::Claude,
                _ => Kind::Note,
            };
            Some(Event { at, kind, text: p.next()?.to_string() })
        })
        .collect()
}

// ---------- the credential, which stays on the host ----------

#[derive(Clone, Copy, PartialEq)]
pub enum Auth {
    /// `sk-ant-api…`, sent as x-api-key.
    Key,
    /// `sk-ant-oat…` from `claude setup-token`, sent as a bearer token.
    OAuth,
}

impl Auth {
    pub fn as_str(self) -> &'static str {
        if self == Auth::OAuth { "oauth" } else { "key" }
    }
}

pub struct Credential {
    pub kind: Auth,
    secret: String,
}

fn key_file() -> PathBuf {
    std::env::home_dir().unwrap_or_default().join(".config/popcorn/claude-key")
}

pub fn credential() -> Option<Credential> {
    let secret = std::env::var("POPCORN_CLAUDE_KEY").ok().or_else(|| fs::read_to_string(key_file()).ok())?.trim().to_string();
    if !secret.starts_with("sk-ant-") {
        return None;
    }
    let kind = if secret.starts_with("sk-ant-oat") { Auth::OAuth } else { Auth::Key };
    Some(Credential { kind, secret })
}

/// The token's kind and its first characters, for showing in settings. Never the whole secret.
pub fn credential_hint() -> Option<String> {
    let c = credential()?;
    let kind = if c.kind == Auth::OAuth { "Long-lived token" } else { "API key" };
    Some(format!("{kind} {}…", c.secret.chars().take(14).collect::<String>()))
}

/// Delete the saved token. Corns lose Claude at once; their next request gets the API's own 401.
pub fn remove_credential() -> Result<(), String> {
    match fs::remove_file(key_file()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Save a key pasted by the user, readable by this user only.
pub fn save_credential(secret: &str) -> Result<(), String> {
    let s = secret.trim();
    if !s.starts_with("sk-ant-") {
        return Err("This is not a Claude token. A Claude token starts with sk-ant-.".into());
    }
    let p = key_file();
    fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut f = OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&p).map_err(|e| e.to_string())?;
    f.write_all(s.as_bytes()).map_err(|e| e.to_string())
}

// ---------- serving a case ----------

pub fn port_path(sock: &Path, port: u32) -> PathBuf {
    PathBuf::from(format!("{}_{port}", sock.display()))
}

/// Most connections a corn may hold open on each port. More are closed at once, so a hostile
/// corn cannot use up the host's threads or memory.
const MAX_CONNS: usize = 64;

/// Counts a connection while it lives.
struct Slot(Arc<std::sync::atomic::AtomicUsize>);

impl Slot {
    fn take(count: &Arc<std::sync::atomic::AtomicUsize>) -> Option<Slot> {
        use std::sync::atomic::Ordering;
        (count.fetch_add(1, Ordering::SeqCst) < MAX_CONNS).then(|| Slot(count.clone())).or_else(|| {
            count.fetch_sub(1, Ordering::SeqCst);
            None
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A request head must arrive within this time.
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);

pub fn serve(sock: &Path, policy: Arc<Policy>, log: Log, dir: PathBuf) -> Result<(), String> {
    let egress = UnixListener::bind(port_path(sock, 1080)).map_err(|e| e.to_string())?;
    let api = UnixListener::bind(port_path(sock, 1081)).map_err(|e| e.to_string())?;
    // ponytail: listeners live until the process exits; a restarted case binds fresh sockets.
    let (l2, d2) = (log.clone(), dir.clone());
    std::thread::spawn(move || {
        let count = Arc::default();
        for c in egress.incoming().flatten() {
            let Some(slot) = Slot::take(&count) else { continue };
            let (policy, log, dir) = (policy.clone(), log.clone(), dir.clone());
            std::thread::spawn(move || {
                let _slot = slot;
                egress_conn(c, &policy, &log, &dir)
            });
        }
    });
    std::thread::spawn(move || {
        let count = Arc::default();
        for c in api.incoming().flatten() {
            let Some(slot) = Slot::take(&count) else { continue };
            let (log, dir) = (l2.clone(), d2.clone());
            std::thread::spawn(move || {
                let _slot = slot;
                if let Err(e) = api_conn(c, &log, &dir) {
                    record(&log, &dir, Event { at: now(), kind: Kind::Blocked, text: format!("Claude API error: {e}") });
                }
            });
        }
    });
    Ok(())
}

fn read_head(r: &mut BufReader<UnixStream>) -> Option<Vec<String>> {
    let mut lines = Vec::new();
    loop {
        let mut l = String::new();
        if r.read_line(&mut l).ok()? == 0 || lines.len() > 200 {
            return None;
        }
        let l = l.trim_end_matches(['\r', '\n']).to_string();
        if l.is_empty() {
            return Some(lines);
        }
        lines.push(l);
    }
}

/// True for addresses on the public internet. Loopback, private, link-local (the cloud metadata
/// service at 169.254.169.254), shared, multicast and other special ranges are on your side of
/// the gate: a corn never reaches them, even for a host you allowed.
pub fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || a == 0
                || (a == 100 && (64..128).contains(&b))
                || a >= 240)
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => public(IpAddr::V4(v4)),
            None => !(v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || v6.is_unique_local() || v6.is_unicast_link_local()),
        },
    }
}

fn egress_conn(c: UnixStream, policy: &Policy, log: &Log, dir: &Path) {
    c.set_read_timeout(Some(HEAD_TIMEOUT)).ok();
    let mut r = BufReader::new(c.try_clone().unwrap());
    let Some(head) = read_head(&mut r) else { return };
    // The tunnel may sit idle for a long time; only the head has a deadline.
    c.set_read_timeout(None).ok();
    let mut w = c;
    let first = head.first().cloned().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let target = target.to_ascii_lowercase();
    let host = target.rsplit_once(':').map_or(target.as_str(), |(h, _)| h).trim_matches(['[', ']']).to_string();
    let literal_internal = host.parse::<IpAddr>().is_ok_and(|ip| !public(ip));
    // Plain-HTTP proxy requests are refused outright: they would let the guest pick any URL per request.
    // An internal address is refused before you are asked. A name is resolved only after you allow
    // it, so a name you deny is never looked up.
    let (ok, why) = if method != "CONNECT" {
        (false, "plain HTTP")
    } else if literal_internal {
        (false, "address on your network")
    } else {
        policy.decide(&target)
    };
    if !ok {
        let what = if method == "CONNECT" { target.clone() } else { format!("{method} {target}") };
        record(log, dir, Event { at: now(), kind: Kind::Blocked, text: format!("{what} ({why})") });
        let _ = w.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    let port: u16 = target.rsplit_once(':').and_then(|(_, p)| p.parse().ok()).unwrap_or(443);
    // Connect only to the addresses checked here, so a name cannot change to an internal address
    // between the check and the connection (DNS rebinding).
    let addrs: Vec<SocketAddr> = (host.as_str(), port).to_socket_addrs().map(|a| a.collect()).unwrap_or_default();
    let open: Vec<SocketAddr> = addrs.iter().copied().filter(|a| public(a.ip())).collect();
    if !addrs.is_empty() && open.is_empty() {
        record(log, dir, Event { at: now(), kind: Kind::Blocked, text: format!("{target} (resolves to an address on your network)") });
        let _ = w.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    let Ok(up) = TcpStream::connect(&open[..]) else {
        let _ = w.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n");
        return;
    };
    record(log, dir, Event { at: now(), kind: Kind::Allowed, text: if why.is_empty() { target.clone() } else { format!("{target} ({why})") } });
    let _ = w.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
    // Any bytes the client sent after the CONNECT head are still in the reader's buffer.
    let mut up_w = up.try_clone().unwrap();
    let buffered = r.buffer().to_vec();
    let _ = up_w.write_all(&buffered);
    let mut guest = w;
    let mut up_r = up;
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut r.into_inner(), &mut up_w);
        let _ = up_w.shutdown(std::net::Shutdown::Write);
    });
    let _ = std::io::copy(&mut up_r, &mut guest);
}

fn tls() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            Arc::new(rustls::ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions().unwrap().with_root_certificates(roots).with_no_client_auth())
        })
        .clone()
}

/// One request per connection: read it, swap the placeholder credential for the real one,
/// send it to the API over TLS, stream the answer back. With no token on the host the request
/// goes out without one, and Claude Code shows the API's own authentication error.
fn api_conn(c: UnixStream, log: &Log, dir: &Path) -> Result<(), String> {
    let cred = credential();
    // The request must arrive in time. The answer may stream for minutes, so the timeout ends there.
    c.set_read_timeout(Some(HEAD_TIMEOUT)).map_err(|e| e.to_string())?;
    let mut r = BufReader::new(c.try_clone().map_err(|e| e.to_string())?);
    let head = read_head(&mut r).ok_or("bad request")?;
    let request_line = head.first().cloned().unwrap_or_default();
    let mut len = 0u64;
    let mut out = format!("{request_line}\r\nHost: {API}\r\nConnection: close\r\n");
    for h in &head[1..] {
        let name = h.split(':').next().unwrap_or("").trim().to_ascii_lowercase();
        match name.as_str() {
            "host" | "connection" | "x-api-key" | "authorization" | "proxy-connection" => {}
            "content-length" => {
                len = h.split(':').nth(1).unwrap_or("0").trim().parse().unwrap_or(0);
                out.push_str(h);
                out.push_str("\r\n");
            }
            _ => {
                out.push_str(h);
                out.push_str("\r\n");
            }
        }
    }
    match cred {
        Some(Credential { kind: Auth::Key, secret }) => out.push_str(&format!("x-api-key: {secret}\r\n")),
        Some(Credential { kind: Auth::OAuth, secret }) => out.push_str(&format!("Authorization: Bearer {secret}\r\n")),
        None => {}
    }
    out.push_str("\r\n");
    if len > 64 << 20 {
        return Err("request too large".into());
    }

    let conn = rustls::ClientConnection::new(tls(), API.try_into().unwrap()).map_err(|e| e.to_string())?;
    let sock = TcpStream::connect((API, 443)).map_err(|e| e.to_string())?;
    let mut s = rustls::StreamOwned::new(conn, sock);
    s.write_all(out.as_bytes()).map_err(|e| e.to_string())?;
    // Stream the body through; never hold it whole in memory.
    let sent = std::io::copy(&mut (&mut r).take(len), &mut s).map_err(|e| e.to_string())?;
    if sent != len {
        return Err("request body cut short".into());
    }
    s.flush().map_err(|e| e.to_string())?;
    c.set_read_timeout(None).ok();
    let path = request_line.split_whitespace().nth(1).unwrap_or("").split('?').next().unwrap_or("").to_string();
    record(log, dir, Event { at: now(), kind: Kind::Claude, text: path });
    let mut guest = c;
    // The API closes the connection when done (Connection: close); a missing close_notify is fine.
    let mut buf = [0u8; 16384];
    loop {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => guest.write_all(&buf[..n]).map_err(|e| e.to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

// ---------- getting files out: pulled by the host, never pushed by the case ----------

/// Read a tar stream from a case without trusting it. Only regular files come through; each
/// path is split into plain components (no empty, ".", ".." or absolute parts), and size limits
/// apply. Links, devices and anything else are skipped, never created. `file` gets the safe path
/// components and a reader for exactly that file's bytes.
pub fn read_tar(mut r: impl Read, mut file: impl FnMut(Vec<String>, &mut dyn Read) -> Result<(), String>) -> Result<(), String> {
    const MAX_FILE: u64 = 50 << 20;
    const MAX_TOTAL: u64 = 500 << 20;
    let mut total = 0u64;
    let mut head = [0u8; 512];
    loop {
        if r.read_exact(&mut head).is_err() || head.iter().all(|&b| b == 0) {
            return Ok(());
        }
        let field = |a: usize, b: usize| String::from_utf8_lossy(&head[a..b]).trim_end_matches('\0').trim().to_string();
        let size = u64::from_str_radix(&field(124, 136), 8).map_err(|_| "bad archive")?;
        let regular = head[156] == b'0' || head[156] == 0;
        let name = match field(345, 500) {
            p if !p.is_empty() && &head[257..262] == b"ustar" => format!("{p}/{}", field(0, 100)),
            _ => field(0, 100),
        };
        let padded = size.div_ceil(512) * 512;
        let parts: Vec<String> = name.split('/').filter(|p| !p.is_empty() && *p != "." && *p != "..").map(String::from).collect();
        if !regular || parts.is_empty() || size > MAX_FILE || total + size > MAX_TOTAL {
            std::io::copy(&mut (&mut r).take(padded), &mut std::io::sink()).map_err(|e| e.to_string())?;
            continue;
        }
        total += size;
        let mut body = (&mut r).take(size);
        file(parts, &mut body)?;
        std::io::copy(&mut body, &mut std::io::sink()).map_err(|e| e.to_string())?;
        std::io::copy(&mut (&mut r).take(padded - size), &mut std::io::sink()).map_err(|e| e.to_string())?;
    }
}

/// A file name or path component made safe for the host file system.
pub fn safe_name(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || "._-+@ ".contains(c) { c } else { '_' }).collect()
}

/// Pull ~/outbox from a running case into `dest` (a quarantine folder): plain files only, names
/// flattened ("sub/dir/report.md" becomes "sub-dir-report.md"), read-only to you, never executable,
/// never overwriting.
pub fn import(r: impl Read, dest: &Path) -> Result<Vec<PathBuf>, String> {
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    read_tar(r, |parts, body| {
        let safe = safe_name(&parts.join("-"));
        let mut path = dest.join(&safe);
        let mut n = 1;
        while path.exists() {
            path = dest.join(format!("{n}-{safe}"));
            n += 1;
        }
        let mut f = OpenOptions::new().create_new(true).write(true).mode(0o600).open(&path).map_err(|e| e.to_string())?;
        std::io::copy(body, &mut f).map_err(|e| e.to_string())?;
        out.push(path);
        Ok(())
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_addresses_are_not_public() {
        for ip in ["127.0.0.1", "10.1.2.3", "172.16.0.1", "192.168.1.1", "169.254.169.254", "100.64.0.1", "0.0.0.0", "224.0.0.1", "255.255.255.255", "::1", "fe80::1", "fd00::1", "::ffff:127.0.0.1", "::ffff:169.254.169.254"] {
            assert!(!public(ip.parse().unwrap()), "{ip} must be refused");
        }
        for ip in ["1.1.1.1", "104.18.0.1", "2606:4700::1111"] {
            assert!(public(ip.parse().unwrap()), "{ip} must be reachable");
        }
    }

    /// One tar entry: name, typeflag, contents.
    fn entry(name: &str, kind: u8, body: &[u8]) -> Vec<u8> {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[124..135].copy_from_slice(format!("{:011o}", body.len()).as_bytes());
        h[156] = kind;
        let mut v = h.to_vec();
        v.extend_from_slice(body);
        v.resize(v.len().div_ceil(512) * 512, 0);
        v
    }

    #[test]
    fn import_keeps_only_plain_files_inside_quarantine() {
        let dest = std::env::temp_dir().join(format!("popcorn-import-{}", std::process::id()));
        let mut tar = Vec::new();
        tar.extend(entry("./report.md", b'0', b"# findings"));
        tar.extend(entry("../../.ssh/authorized_keys", b'0', b"evil"));
        tar.extend(entry("/etc/passwd", b'0', b"evil"));
        tar.extend(entry("link", b'2', b""));
        tar.extend(entry("dev", b'3', b""));
        tar.extend(entry("sub/ioc.json", b'0', b"{}"));
        tar.extend([0u8; 1024]);
        let got = import(&tar[..], &dest).unwrap();
        let names: Vec<String> = got.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(sorted, [".ssh-authorized_keys", "etc-passwd", "report.md", "sub-ioc.json"], "names are flattened, never paths");
        assert!(got.iter().all(|p| p.parent() == Some(dest.as_path())), "everything stays in the quarantine folder");
        assert!(names.contains(&"report.md".to_string()) && names.contains(&"sub-ioc.json".to_string()));
        assert!(!names.iter().any(|n| n == "link" || n == "dev"), "links and devices are never created");
        for p in &got {
            let mode = std::os::unix::fs::PermissionsExt::mode(&fs::metadata(p).unwrap().permissions());
            assert_eq!(mode & 0o777, 0o600);
        }
        fs::remove_dir_all(&dest).ok();
    }
}
