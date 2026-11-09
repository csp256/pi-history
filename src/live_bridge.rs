//! Authenticated local transport to the optional Pi companion. Never edits JSONL or starts Pi.
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const MAX_MESSAGE_BYTES: usize = 32 * 1024;
const MAX_RESPONSE_BYTES: usize = 8192;

#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    version: u32,
    pub pid: u32,
    port: u16,
    token: String,
    pub session_file: PathBuf,
    session_id: String,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("pid", &self.pid)
            .field("session_file", &self.session_file)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveTarget {
    pub endpoint: Endpoint,
    pub name: Option<String>,
    pub busy: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Steer,
    FollowUp,
}
impl Delivery {
    fn wire(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::FollowUp => "followUp",
        }
    }
}

#[derive(Debug)]
pub struct SendFailure {
    pub message: String,
    /// A lost acknowledgement cannot prove that Pi did not already accept the message.
    pub uncertain: bool,
}

pub fn new_request_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

pub fn bridge_directory() -> Result<PathBuf, String> {
    std::env::var_os("PI_HISTORY_BRIDGE_DIR")
        .map(PathBuf::from)
        .or_else(|| home::home_dir().map(|p| p.join(".cache/pi-history/bridge")))
        .ok_or_else(|| "Cannot determine the pi-history bridge directory".to_string())
}

fn private_metadata(path: &Path, directory: bool) -> Result<std::fs::Metadata, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err("Bridge paths must be regular files/directories, not symlinks".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(
                "Bridge credentials must be private (directory 0700, files 0600)".to_string(),
            );
        }
    }
    Ok(metadata)
}

fn request(endpoint: &Endpoint, mut body: Value) -> Result<Value, String> {
    body["version"] = json!(1);
    body["token"] = json!(endpoint.token);
    body["sessionFile"] = json!(endpoint.session_file);
    body["sessionId"] = json!(endpoint.session_id);
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), endpoint.port);
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_millis(300))
        .map_err(|e| format!("Cannot reach Pi bridge: {e}"))?;
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    socket
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let mut bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    socket
        .write_all(&bytes)
        .map_err(|e| format!("Bridge write failed: {e}"))?;
    let mut response = String::new();
    BufReader::new(socket.take((MAX_RESPONSE_BYTES + 1) as u64))
        .read_line(&mut response)
        .map_err(|e| format!("Bridge acknowledgement failed: {e}"))?;
    if response.len() > MAX_RESPONSE_BYTES || !response.ends_with('\n') {
        return Err("Incomplete or oversized bridge response".to_string());
    }
    let result: Value =
        serde_json::from_str(&response).map_err(|_| "Invalid bridge response".to_string())?;
    if result["version"] != 1 {
        return Err("Unsupported bridge protocol version".to_string());
    }
    if result["ok"] != true {
        return Err(result["error"]
            .as_str()
            .unwrap_or("Pi rejected the message")
            .to_string());
    }
    Ok(result)
}

pub fn discover(session_file: &Path) -> Result<LiveTarget, String> {
    discover_in(&bridge_directory()?, session_file)
}

fn canonical_session_file(path: &Path) -> Result<PathBuf, std::io::Error> {
    let canonical = std::fs::canonicalize(path)?;
    // Node's realpath uses ordinary Windows paths; Rust adds a verbatim prefix.
    #[cfg(windows)]
    {
        let text = canonical.to_string_lossy();
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{unc}")));
        }
        if let Some(path) = text.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(path));
        }
    }
    Ok(canonical)
}

fn discover_in(directory: &Path, session_file: &Path) -> Result<LiveTarget, String> {
    const OFFLINE: &str = "No running Pi bridge for this session. Install the companion, then /reload or restart Pi in that session.";
    let session_file =
        canonical_session_file(session_file).map_err(|e| format!("Cannot resolve session: {e}"))?;
    if !directory.exists() {
        return Err(OFFLINE.to_string());
    }
    private_metadata(directory, true)?;
    let mut targets = Vec::new();
    let mut candidates = 0;
    let entries = std::fs::read_dir(directory).map_err(|e| e.to_string())?;
    for (index, entry) in entries.enumerate() {
        if index >= 512 {
            return Err(
                "Too many bridge records; remove stale records while Pi is stopped".to_string(),
            );
        }
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(metadata) = private_metadata(&path, false) else {
            continue;
        };
        if metadata.len() > 8192 {
            continue;
        }
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let Ok(endpoint) = serde_json::from_reader::<_, Endpoint>(file.take(8193)) else {
            continue;
        };
        if endpoint.version != 1
            || endpoint.session_file != session_file
            || endpoint.port == 0
            || endpoint.token.len() != 64
        {
            continue;
        }
        candidates += 1;
        if candidates > 16 {
            return Err("Too many bridge records for this session; close duplicate Pi instances and remove stale records".to_string());
        }
        let Ok(status) = request(&endpoint, json!({"type":"status"})) else {
            continue;
        };
        if status["sessionFile"] != json!(session_file)
            || status["sessionId"] != endpoint.session_id
            || status["pid"] != endpoint.pid
        {
            continue;
        }
        let Some(busy) = status["busy"].as_bool() else {
            continue;
        };
        targets.push(LiveTarget {
            endpoint,
            name: status["name"].as_str().map(str::to_string),
            busy,
        });
    }
    match targets.len() {
        0 => Err(OFFLINE.to_string()),
        1 => Ok(targets.remove(0)),
        _ => Err("This session is open in multiple running Pi instances. Close duplicates before sending.".to_string()),
    }
}

pub fn send(
    target: &LiveTarget,
    request_id: &str,
    message: &str,
    delivery: Delivery,
) -> Result<String, SendFailure> {
    if message.trim().is_empty() || message.len() > MAX_MESSAGE_BYTES || message.contains('\0') {
        return Err(SendFailure {
            message: "Message must be nonempty text of at most 32 KiB".to_string(),
            uncertain: false,
        });
    }
    let response = request(&target.endpoint, json!({"type":"send", "requestId":request_id, "message":message, "delivery":delivery.wire()}))
        .map_err(|message| SendFailure { message, uncertain: true })?;
    if response["requestId"] != request_id || response["disposition"] != "accepted" {
        return Err(SendFailure {
            message: "Unrecognized message acknowledgement".to_string(),
            uncertain: true,
        });
    }
    match response["delivery"].as_str() {
        Some("prompt") => Ok("Pi accepted message for dispatch".to_string()),
        Some("steer") => Ok("Pi accepted steering message".to_string()),
        Some("followUp") => Ok("Pi accepted follow-up message".to_string()),
        _ => Err(SendFailure {
            message: "Unrecognized delivery acknowledgement".to_string(),
            uncertain: true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    fn private_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        dir
    }
    fn record(directory: &Path, session: &Path, port: u16, pid: u32) -> Endpoint {
        let value = json!({"version":1,"port":port,"pid":pid,"token":"a".repeat(64),"sessionFile":session,"sessionId":"test-session"});
        let path = directory.join(format!("{pid}.json"));
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        serde_json::from_value(value).unwrap()
    }
    fn mock(
        directory: &Path,
        session: &Path,
        pid: u32,
        count: usize,
    ) -> (
        Endpoint,
        Arc<Mutex<Vec<Value>>>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint = record(
            directory,
            session,
            listener.local_addr().unwrap().port(),
            pid,
        );
        let target = endpoint.clone();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let requests = seen.clone();
        let worker = std::thread::spawn(move || {
            for _ in 0..count {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let body: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(body["token"], target.token);
                assert_eq!(body["sessionFile"], json!(target.session_file));
                let reply = if body["type"] == "status" {
                    json!({"version":1,"ok":true,"pid":pid,"sessionFile":target.session_file,"sessionId":target.session_id,"busy":true,"name":"Live test"})
                } else {
                    json!({"version":1,"ok":true,"requestId":body["requestId"],"disposition":"accepted","delivery":body["delivery"]})
                };
                requests.lock().unwrap().push(body);
                writeln!(stream, "{reply}").unwrap();
            }
        });
        (endpoint, seen, worker)
    }
    #[test]
    fn discovers_exact_saved_session_and_sends_unicode_without_editing_history() {
        let dir = private_dir();
        let session = dir.path().join("session.jsonl");
        std::fs::write(&session, "original\n").unwrap();
        let session = canonical_session_file(&session).unwrap();
        let (_, seen, worker) = mock(dir.path(), &session, 42, 3);
        let target = discover_in(dir.path(), &session).unwrap();
        assert!(target.busy);
        assert!(send(&target, "unique-one", "Hello λ\n世界", Delivery::Steer).is_ok());
        assert!(send(&target, "unique-two", "Later", Delivery::FollowUp).is_ok());
        worker.join().unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[1]["message"], "Hello λ\n世界");
        assert_eq!(seen[2]["delivery"], "followUp");
        assert_eq!(std::fs::read_to_string(session).unwrap(), "original\n");
        assert!(!format!("{target:?}").contains(&"a".repeat(64)));
    }
    #[test]
    fn refuses_ambiguous_running_instances() {
        let dir = private_dir();
        let session = dir.path().join("session.jsonl");
        std::fs::write(&session, "x").unwrap();
        let session = canonical_session_file(&session).unwrap();
        let (_, _, one) = mock(dir.path(), &session, 1, 1);
        let (_, _, two) = mock(dir.path(), &session, 2, 1);
        assert!(
            discover_in(dir.path(), &session)
                .unwrap_err()
                .contains("multiple")
        );
        one.join().unwrap();
        two.join().unwrap();
    }
    #[test]
    fn offline_invalid_and_other_session_records_never_send() {
        let dir = private_dir();
        let session = dir.path().join("session.jsonl");
        std::fs::write(&session, "x").unwrap();
        record(dir.path(), Path::new("/not/the/selected/session"), 9, 1);
        std::fs::write(dir.path().join("malformed.json"), "not JSON").unwrap();
        assert!(
            discover_in(dir.path(), &session)
                .unwrap_err()
                .contains("No running")
        );
        assert!(discover_in(&dir.path().join("missing"), &session).is_err());
    }
    #[test]
    fn rejects_empty_and_oversized_text_before_connecting() {
        let dir = private_dir();
        let endpoint = record(dir.path(), Path::new("/unused"), 9, 1);
        let target = LiveTarget {
            endpoint,
            name: None,
            busy: false,
        };
        for text in [
            " ".to_string(),
            "x".repeat(MAX_MESSAGE_BYTES + 1),
            "bad\0text".to_string(),
        ] {
            assert!(
                !send(&target, "id", &text, Delivery::Steer)
                    .unwrap_err()
                    .uncertain
            );
        }
    }
    #[test]
    fn lost_acknowledgement_is_reported_as_uncertain() {
        let dir = private_dir();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint = record(
            dir.path(),
            Path::new("/unused"),
            listener.local_addr().unwrap().port(),
            1,
        );
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).unwrap();
        });
        let target = LiveTarget {
            endpoint,
            name: None,
            busy: false,
        };
        assert!(
            send(&target, "stable-id", "hello", Delivery::Steer)
                .unwrap_err()
                .uncertain
        );
        worker.join().unwrap();
    }
    #[test]
    fn oversized_response_is_bounded() {
        let dir = private_dir();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint = record(
            dir.path(),
            Path::new("/unused"),
            listener.local_addr().unwrap().port(),
            1,
        );
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let _ = writeln!(stream, "{}", "x".repeat(MAX_RESPONSE_BYTES + 10));
        });
        assert!(
            request(&endpoint, json!({"type":"status"}))
                .unwrap_err()
                .contains("oversized")
        );
        worker.join().unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn refuses_insecure_directories_and_symlink_credentials() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = private_dir();
        let session = dir.path().join("session.jsonl");
        std::fs::write(&session, "x").unwrap();
        let record_path = dir.path().join("1.json");
        record(dir.path(), &session, 9, 1);
        std::fs::rename(&record_path, dir.path().join("secret")).unwrap();
        symlink(dir.path().join("secret"), &record_path).unwrap();
        assert!(
            discover_in(dir.path(), &session)
                .unwrap_err()
                .contains("No running")
        );
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            discover_in(dir.path(), &session)
                .unwrap_err()
                .contains("private")
        );
    }
}
