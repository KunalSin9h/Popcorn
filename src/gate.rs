//! The only way out of a case. The guest dials two vsock ports, which Firecracker turns into
//! unix sockets here: 1080 is an HTTP CONNECT proxy that asks you about every new host, 1081 is
//! the Claude API, where the real credential is added on the host so it never exists in the VM.
use crate::sandbox::Mode;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
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

/// Save a key pasted by the user, readable by this user only.
pub fn save_credential(secret: &str) -> Result<(), String> {
    let s = secret.trim();
    if !s.starts_with("sk-ant-") {
        return Err("That does not look like a Claude key (it should start with sk-ant-)".into());
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

pub fn serve(sock: &Path, policy: Arc<Policy>, log: Log, dir: PathBuf) -> Result<(), String> {
    let egress = UnixListener::bind(port_path(sock, 1080)).map_err(|e| e.to_string())?;
    let api = UnixListener::bind(port_path(sock, 1081)).map_err(|e| e.to_string())?;
    // ponytail: listeners live until the process exits; a restarted case binds fresh sockets.
    let (l2, d2) = (log.clone(), dir.clone());
    std::thread::spawn(move || {
        for c in egress.incoming().flatten() {
            let (policy, log, dir) = (policy.clone(), log.clone(), dir.clone());
            std::thread::spawn(move || egress_conn(c, &policy, &log, &dir));
        }
    });
    std::thread::spawn(move || {
        for c in api.incoming().flatten() {
            let (log, dir) = (l2.clone(), d2.clone());
            std::thread::spawn(move || {
                if credential().is_none() {
                    // Tell Claude Code why, in the API's own error format, instead of a dropped connection.
                    record(&log, &dir, Event { at: now(), kind: Kind::Blocked, text: "Claude API (not connected)".into() });
                    let body = r#"{"type":"error","error":{"type":"authentication_error","message":"Claude is not connected to this corn. In Popcorn, select Connect next to Claude."}}"#;
                    let mut c = c;
                    let _ = write!(c, "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    return;
                }
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

fn egress_conn(c: UnixStream, policy: &Policy, log: &Log, dir: &Path) {
    let mut r = BufReader::new(c.try_clone().unwrap());
    let Some(head) = read_head(&mut r) else { return };
    let mut w = c;
    let first = head.first().cloned().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let target = target.to_ascii_lowercase();
    let host = target.rsplit_once(':').map_or(target.as_str(), |(h, _)| h).to_string();
    // Plain-HTTP proxy requests are refused outright: they would let the guest pick any URL per request.
    let (ok, why) = if method == "CONNECT" { policy.decide(&target) } else { (false, "plain HTTP") };
    if !ok {
        let what = if method == "CONNECT" { target.clone() } else { format!("{method} {target}") };
        record(log, dir, Event { at: now(), kind: Kind::Blocked, text: format!("{what} ({why})") });
        let _ = w.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    let port: u16 = target.rsplit_once(':').and_then(|(_, p)| p.parse().ok()).unwrap_or(443);
    let Ok(up) = TcpStream::connect((host.as_str(), port)) else {
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
/// send it to the API over TLS, stream the answer back.
fn api_conn(c: UnixStream, log: &Log, dir: &Path) -> Result<(), String> {
    let cred = credential().ok_or("no credential on the host")?;
    let mut r = BufReader::new(c.try_clone().map_err(|e| e.to_string())?);
    let head = read_head(&mut r).ok_or("bad request")?;
    let request_line = head.first().cloned().unwrap_or_default();
    let mut len = 0usize;
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
    match cred.kind {
        Auth::Key => out.push_str(&format!("x-api-key: {}\r\n", cred.secret)),
        Auth::OAuth => out.push_str(&format!("Authorization: Bearer {}\r\n", cred.secret)),
    }
    out.push_str("\r\n");
    let mut body = vec![0; len.min(64 << 20)];
    r.read_exact(&mut body).map_err(|e| e.to_string())?;

    let conn = rustls::ClientConnection::new(tls(), API.try_into().unwrap()).map_err(|e| e.to_string())?;
    let sock = TcpStream::connect((API, 443)).map_err(|e| e.to_string())?;
    let mut s = rustls::StreamOwned::new(conn, sock);
    s.write_all(out.as_bytes()).map_err(|e| e.to_string())?;
    s.write_all(&body).map_err(|e| e.to_string())?;
    s.flush().map_err(|e| e.to_string())?;
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
