//! Test doubles: a loopback HTTP server that serves a manifest and update files,
//! throwaway signing keys, and builders for the update archives.
//!
//! The server answers `GET` for the paths it was given (anything else is 404),
//! with `Connection: close`. A path can answer with another status, redirect,
//! answer late (to hit a time limit), or announce a length and close early (an
//! interrupted download).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use semver::Version;

use crate::install::InstallRoute;
use crate::manifest::PlatformKey;
use crate::release::{SigningKey, generate_keys};
use crate::updater::UpdaterConfig;

#[derive(Clone, Debug)]
enum Route {
    Body {
        status: u16,
        body: Vec<u8>,
        /// Bytes sent before the connection is closed (the length announced is
        /// the whole body).
        sent: Option<usize>,
        delay: Option<Duration>,
        /// The body is sent in pieces of this many bytes, with this pause
        /// before each piece after the first.
        trickle: Option<(usize, Duration)>,
    },
    Redirect(String),
}

#[derive(Debug, Default)]
struct State {
    routes: BTreeMap<String, Route>,
    offline: bool,
    log: Vec<String>,
}

/// A running HTTP test server. Dropping it stops it.
#[derive(Debug)]
pub struct HttpServer {
    address: SocketAddr,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl HttpServer {
    /// A server with no paths.
    ///
    /// # Panics
    ///
    /// When no loopback port can be bound.
    #[must_use]
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let address = listener.local_addr().expect("an address");
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let state = Arc::clone(&state);
                    std::thread::spawn(move || serve(stream, &state));
                }
            })
        };
        Self {
            address,
            state,
            stop,
            thread: Some(thread),
        }
    }

    /// The address of `path` (`"latest.json"` → `http://127.0.0.1:<port>/latest.json`).
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("http://{}/{}", self.address, path.trim_start_matches('/'))
    }

    /// `path` answers 200 with `body`.
    pub fn serve(&self, path: &str, body: impl Into<Vec<u8>>) {
        self.route(
            path,
            Route::Body {
                status: 200,
                body: body.into(),
                sent: None,
                delay: None,
                trickle: None,
            },
        );
    }

    /// `path` answers 200 with `body`, sent `chunk` bytes at a time with `pause`
    /// between the pieces (a slow link, or with a long pause a server that goes
    /// quiet in the middle of the body).
    pub fn serve_trickled(
        &self,
        path: &str,
        body: impl Into<Vec<u8>>,
        chunk: usize,
        pause: Duration,
    ) {
        self.route(
            path,
            Route::Body {
                status: 200,
                body: body.into(),
                sent: None,
                delay: None,
                trickle: Some((chunk.max(1), pause)),
            },
        );
    }

    /// `path` answers `status` with an empty body.
    pub fn serve_status(&self, path: &str, status: u16) {
        self.route(
            path,
            Route::Body {
                status,
                body: Vec::new(),
                sent: None,
                delay: None,
                trickle: None,
            },
        );
    }

    /// `path` announces `body`'s length but closes after `sent` bytes.
    pub fn serve_truncated(&self, path: &str, body: impl Into<Vec<u8>>, sent: usize) {
        self.route(
            path,
            Route::Body {
                status: 200,
                body: body.into(),
                sent: Some(sent),
                delay: None,
                trickle: None,
            },
        );
    }

    /// `path` answers 200 with `body` after `delay`.
    pub fn serve_delayed(&self, path: &str, body: impl Into<Vec<u8>>, delay: Duration) {
        self.route(
            path,
            Route::Body {
                status: 200,
                body: body.into(),
                sent: None,
                delay: Some(delay),
                trickle: None,
            },
        );
    }

    /// `path` redirects (302) to `target` on this server.
    pub fn redirect(&self, path: &str, target: &str) {
        let target = self.url(target);
        self.route(path, Route::Redirect(target));
    }

    /// While on, every connection is closed before an answer.
    pub fn set_offline(&self, offline: bool) {
        self.lock().offline = offline;
    }

    /// Every request so far, as `GET /path`.
    #[must_use]
    pub fn requests(&self) -> Vec<String> {
        self.lock().log.clone()
    }

    fn route(&self, path: &str, route: Route) {
        let path = format!("/{}", path.trim_start_matches('/'));
        self.lock().routes.insert(path, route);
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop.
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(stream: TcpStream, state: &Mutex<State>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(stream) => stream,
        Err(_) => return,
    });
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("/").to_owned();
    // Skip the headers.
    loop {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) | Err(_) => break,
            Ok(_) if header.trim().is_empty() => break,
            Ok(_) => {}
        }
    }
    let (offline, route) = {
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.log.push(format!("{method} {path}"));
        let route = state
            .routes
            .get(path.split('?').next().unwrap_or(""))
            .cloned();
        (state.offline, route)
    };
    if offline {
        return;
    }
    let mut stream = stream;
    let route = match (method.as_str(), route) {
        ("GET", Some(route)) => route,
        ("GET", None) => Route::Body {
            status: 404,
            body: Vec::new(),
            sent: None,
            delay: None,
            trickle: None,
        },
        _ => Route::Body {
            status: 405,
            body: Vec::new(),
            sent: None,
            delay: None,
            trickle: None,
        },
    };
    match route {
        Route::Redirect(target) => {
            let _ = write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
        Route::Body {
            status,
            body,
            sent,
            delay,
            trickle,
        } => {
            if let Some(delay) = delay {
                std::thread::sleep(delay);
            }
            let head = format!(
                "HTTP/1.1 {status} {}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                reason(status),
                body.len()
            );
            let sent = sent.unwrap_or(body.len()).min(body.len());
            let _ = stream
                .write_all(head.as_bytes())
                .and_then(|()| match trickle {
                    None => stream.write_all(&body[..sent]),
                    Some((chunk, pause)) => {
                        for (index, piece) in body[..sent].chunks(chunk).enumerate() {
                            if index > 0 {
                                std::thread::sleep(pause);
                            }
                            stream.write_all(piece)?;
                            stream.flush()?;
                        }
                        Ok(())
                    }
                })
                .and_then(|()| stream.flush());
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        302 => "Found",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

/// A throwaway, unencrypted signing key.
#[derive(Debug)]
pub struct TestKey {
    key: SigningKey,
}

impl TestKey {
    /// A new key.
    ///
    /// # Panics
    ///
    /// When key generation fails.
    #[must_use]
    pub fn generate() -> Self {
        let keys = generate_keys("").expect("a key pair");
        let key = SigningKey::open(&keys.secret_key_base64, "").expect("the new key opens");
        Self { key }
    }

    /// The public key (base64 key line).
    #[must_use]
    pub fn public_key(&self) -> String {
        self.key.public_key()
    }

    /// A manifest signature of `data` for `version`.
    ///
    /// # Panics
    ///
    /// When `version` is not a version.
    #[must_use]
    pub fn sign(&self, data: &[u8], version: &str) -> String {
        self.key
            .sign(data, "update", version)
            .expect("signing in memory works")
    }

    /// A manifest signature of `data` with the given trusted comment.
    ///
    /// # Panics
    ///
    /// Never in practice: the data is in memory.
    #[must_use]
    pub fn sign_with_comment(&self, data: &[u8], trusted_comment: &str) -> String {
        self.key
            .sign_with_comment(data, trusted_comment)
            .expect("signing in memory works")
    }
}

/// A `latest.json` for `version` with the given entries (`(key, url, signature)`).
#[must_use]
pub fn manifest_json(
    version: &str,
    notes: Option<&str>,
    platforms: &[(PlatformKey, String, String)],
) -> String {
    let platforms: serde_json::Map<String, serde_json::Value> = platforms
        .iter()
        .map(|(key, url, signature)| {
            (
                key.as_str().to_owned(),
                serde_json::json!({ "url": url, "signature": signature }),
            )
        })
        .collect();
    serde_json::json!({
        "version": version,
        "notes": notes,
        "pub_date": "2026-11-16T00:00:00Z",
        "platforms": platforms,
    })
    .to_string()
}

/// Updater settings for tests: `endpoint`, `public_key`, running `current`, on
/// `route`, downloading into `download_dir`.
///
/// # Panics
///
/// When `current` is not a version.
#[must_use]
pub fn config(
    endpoint: String,
    public_key: String,
    current: &str,
    route: InstallRoute,
    download_dir: &Path,
) -> UpdaterConfig {
    UpdaterConfig {
        endpoint,
        public_key,
        current_version: Version::parse(current).expect("a version"),
        route,
        download_dir: download_dir.to_path_buf(),
        check_timeout: Duration::from_secs(5),
        download_idle_timeout: Duration::from_secs(5),
    }
}

/// A portable archive like the release's: `<top>/shellune.exe` with `exe`, and a
/// `LICENSE` beside it.
///
/// # Panics
///
/// When writing to memory fails.
#[must_use]
pub fn portable_zip(top: &str, exe: &[u8]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in [("LICENSE", b"license".as_slice()), ("shellune.exe", exe)] {
        writer
            .start_file(format!("{top}/{name}"), options)
            .expect("a zip entry");
        writer.write_all(data).expect("zip data");
    }
    writer.finish().expect("a zip").into_inner()
}

/// A macOS update archive: `<name>.app/Contents/MacOS/shellune` with `exe`, and an
/// `Info.plist`, gzip-compressed.
///
/// # Panics
///
/// When writing to memory fails.
#[must_use]
pub fn app_tar_gz(name: &str, exe: &[u8]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    let mut add = |path: String, data: &[u8], mode: u32| {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        builder
            .append_data(&mut header, path, data)
            .expect("a tar entry");
    };
    add(
        format!("{name}.app/Contents/Info.plist"),
        b"<plist/>",
        0o644,
    );
    add(format!("{name}.app/Contents/MacOS/shellune"), exe, 0o755);
    builder
        .into_inner()
        .expect("a tar")
        .finish()
        .expect("a gzip stream")
}

/// A fresh empty folder under the system temporary folder, removed on drop.
#[derive(Debug)]
pub struct TempFolder {
    path: PathBuf,
}

impl TempFolder {
    /// A new folder.
    ///
    /// # Panics
    ///
    /// When it cannot be made.
    #[must_use]
    pub fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "shellune-update-test-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&path).expect("a temporary folder");
        Self { path }
    }

    /// Its path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFolder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
