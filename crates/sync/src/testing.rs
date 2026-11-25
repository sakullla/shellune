//! An in-process WebDAV server for tests: real HTTP on a loopback port, the few
//! methods sync uses, Basic authentication and entity tags.
//!
//! It understands `PROPFIND` (depth 0), `MKCOL`, `GET`, `PUT` (with `If-Match` and
//! `If-None-Match: *`) and `DELETE`, and answers every request with
//! `Connection: close`. Switches make it refuse writes ([`WebDavServer::set_read_only`]),
//! drop every connection as an unreachable server would ([`WebDavServer::set_offline`]),
//! leave the entity tags out ([`WebDavServer::set_etags`]) or fail the next write
//! with 412 ([`WebDavServer::fail_next_put`]).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use base64::Engine as _;

/// The path every test account lives under.
const ROOT: &str = "/dav/";

#[derive(Debug, Default)]
struct Files {
    /// Path (decoded, from `/`) to content and entity tag number.
    files: BTreeMap<String, (Vec<u8>, u64)>,
    /// Collection paths, each ending with `/`.
    collections: BTreeSet<String>,
    next_tag: u64,
    authorization: Option<String>,
    offline: bool,
    read_only: bool,
    no_etags: bool,
    fail_next_put: bool,
    /// Replaces the file a GET read once this many more GETs were served: another
    /// machine writing in between this one's read and its write.
    change_after_gets: Option<(usize, Vec<u8>)>,
    log: Vec<String>,
}

/// A running WebDAV test server. Dropping it stops it.
#[derive(Debug)]
pub struct WebDavServer {
    address: SocketAddr,
    state: Arc<Mutex<Files>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WebDavServer {
    /// A server whose account is `username` / `password`, with an empty account
    /// folder at [`Self::url`].
    ///
    /// # Panics
    ///
    /// When no loopback port can be bound.
    #[must_use]
    pub fn start(username: &str, password: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let address = listener.local_addr().expect("an address");
        let token =
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
        let state = Arc::new(Mutex::new(Files {
            collections: BTreeSet::from(["/".to_owned(), ROOT.to_owned()]),
            authorization: Some(format!("Basic {token}")),
            ..Files::default()
        }));
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

    /// The account's WebDAV address.
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://{}{ROOT}", self.address)
    }

    /// While on, every connection is closed before an answer, as when the network
    /// is down.
    pub fn set_offline(&self, offline: bool) {
        self.lock().offline = offline;
    }

    /// While on, every write (`PUT`, `MKCOL`, `DELETE`) is refused with 403.
    pub fn set_read_only(&self, read_only: bool) {
        self.lock().read_only = read_only;
    }

    /// While off, no entity tag is reported (and `If-Match` never matches).
    pub fn set_etags(&self, etags: bool) {
        self.lock().no_etags = !etags;
    }

    /// Replaces the stored file with `bytes` right after the `gets`-th GET from
    /// now: another machine writing between this one's read and its write.
    pub fn change_after_gets(&self, gets: usize, bytes: &[u8]) {
        self.lock().change_after_gets = Some((gets, bytes.to_vec()));
    }

    /// The next `PUT` is answered with 412 as if the file had just changed.
    pub fn fail_next_put(&self) {
        self.lock().fail_next_put = true;
    }

    /// A file's content, by path under the account folder (`Shellune/x.json`).
    #[must_use]
    pub fn file(&self, path: &str) -> Option<Vec<u8>> {
        self.lock()
            .files
            .get(&format!("{ROOT}{path}"))
            .map(|(bytes, _)| bytes.clone())
    }

    /// Writes a file as another client would (its folder must exist).
    pub fn put_file(&self, path: &str, bytes: &[u8]) {
        let mut state = self.lock();
        state.next_tag += 1;
        let tag = state.next_tag;
        state
            .files
            .insert(format!("{ROOT}{path}"), (bytes.to_vec(), tag));
    }

    /// Whether a folder exists, by path under the account folder (`a/b`).
    #[must_use]
    pub fn has_folder(&self, path: &str) -> bool {
        let path = format!("{ROOT}{}/", path.trim_matches('/'));
        self.lock().collections.contains(&path)
    }

    /// The paths of every file, under the account folder.
    #[must_use]
    pub fn files(&self) -> Vec<String> {
        self.lock()
            .files
            .keys()
            .filter_map(|path| path.strip_prefix(ROOT).map(str::to_owned))
            .collect()
    }

    /// Every request answered so far, as `METHOD path`.
    #[must_use]
    pub fn requests(&self) -> Vec<String> {
        self.lock().log.clone()
    }

    fn lock(&self) -> MutexGuard<'_, Files> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for WebDavServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop.
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

struct Answer {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

impl Answer {
    fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }
}

fn serve(stream: TcpStream, state: &Mutex<Files>) {
    let offline = state.lock().map(|state| state.offline).unwrap_or_default();
    if offline {
        let _ = stream.shutdown(std::net::Shutdown::Both);
        return;
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let Some(request) = read_request(&mut reader) else {
        return;
    };
    let answer = {
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .log
            .push(format!("{} {}", request.method, request.path));
        handle(&mut state, &request)
    };
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nConnection: close\r\nContent-Length: {}\r\n",
        answer.status,
        reason(answer.status),
        answer.body.len()
    );
    for (name, value) in &answer.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = writer.write_all(head.as_bytes());
    let _ = writer.write_all(&answer.body);
    let _ = writer.flush();
}

fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?;
    let raw_path = target.split('?').next().unwrap_or(target);
    // A proxy-style absolute target is reduced to its path.
    let raw_path = raw_path
        .find("://")
        .and_then(|scheme| {
            raw_path[scheme + 3..]
                .find('/')
                .map(|slash| &raw_path[scheme + 3 + slash..])
        })
        .unwrap_or(raw_path);
    let path = decode(raw_path);
    let mut headers = BTreeMap::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let mut body = Vec::new();
    if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
    {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).ok()?;
            let size = usize::from_str_radix(size.trim().split(';').next()?, 16).ok()?;
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk).ok()?;
            if size == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..size]);
        }
    } else if let Some(length) = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    {
        body = vec![0; length];
        reader.read_exact(&mut body).ok()?;
    }
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}

fn handle(state: &mut Files, request: &Request) -> Answer {
    if state.authorization.as_deref() != request.headers.get("authorization").map(String::as_str) {
        let mut answer = Answer::status(401);
        answer
            .headers
            .push(("WWW-Authenticate", "Basic realm=\"test\"".to_owned()));
        return answer;
    }
    let path = request.path.clone();
    let as_collection = if path.ends_with('/') {
        path.clone()
    } else {
        format!("{path}/")
    };
    let parent_exists = || {
        let trimmed = path.trim_end_matches('/');
        let parent = &trimmed[..=trimmed.rfind('/').unwrap_or(0)];
        state.collections.contains(parent)
    };
    match request.method.as_str() {
        "PROPFIND" => {
            if state.collections.contains(&as_collection) {
                multistatus(&as_collection, None, true)
            } else if let Some((_, tag)) = state.files.get(&path) {
                let etag = (!state.no_etags).then(|| format!("\"e{tag}\""));
                multistatus(&path, etag.as_deref(), false)
            } else {
                Answer::status(404)
            }
        }
        "MKCOL" => {
            if state.read_only {
                Answer::status(403)
            } else if state.collections.contains(&as_collection) || state.files.contains_key(&path)
            {
                Answer::status(405)
            } else if !parent_exists() {
                Answer::status(409)
            } else {
                state.collections.insert(as_collection);
                Answer::status(201)
            }
        }
        "GET" => match state.files.get(&path) {
            Some((bytes, tag)) => {
                let mut answer = Answer::status(200);
                answer.body.clone_from(bytes);
                if !state.no_etags {
                    answer.headers.push(("ETag", format!("\"e{tag}\"")));
                }
                if let Some((left, replacement)) = state.change_after_gets.take() {
                    if left <= 1 {
                        state.next_tag += 1;
                        let tag = state.next_tag;
                        state.files.insert(path, (replacement, tag));
                    } else {
                        state.change_after_gets = Some((left - 1, replacement));
                    }
                }
                answer
            }
            None => Answer::status(404),
        },
        "PUT" => {
            if state.read_only {
                return Answer::status(403);
            }
            if !parent_exists() {
                return Answer::status(409);
            }
            if std::mem::take(&mut state.fail_next_put) {
                return Answer::status(412);
            }
            let current = state.files.get(&path).map(|(_, tag)| format!("\"e{tag}\""));
            if let Some(expected) = request.headers.get("if-match") {
                let matches = !state.no_etags
                    && current.as_deref().is_some_and(|tag| {
                        expected == "*" || expected.split(',').any(|item| item.trim() == tag)
                    });
                if !matches {
                    return Answer::status(412);
                }
            }
            if request
                .headers
                .get("if-none-match")
                .is_some_and(|value| value.trim() == "*")
                && current.is_some()
            {
                return Answer::status(412);
            }
            state.next_tag += 1;
            let tag = state.next_tag;
            let existed = state
                .files
                .insert(path, (request.body.clone(), tag))
                .is_some();
            let mut answer = Answer::status(if existed { 204 } else { 201 });
            if !state.no_etags {
                answer.headers.push(("ETag", format!("\"e{tag}\"")));
            }
            answer
        }
        "DELETE" => {
            if state.read_only {
                Answer::status(403)
            } else if state.files.remove(&path).is_some() {
                Answer::status(204)
            } else {
                Answer::status(404)
            }
        }
        _ => Answer::status(405),
    }
}

fn multistatus(path: &str, etag: Option<&str>, collection: bool) -> Answer {
    let resource = if collection {
        "<d:resourcetype><d:collection/></d:resourcetype>"
    } else {
        "<d:resourcetype/>"
    };
    let etag = etag.map_or_else(String::new, |etag| {
        format!("<d:getetag>{}</d:getetag>", etag.replace('"', "&quot;"))
    });
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>{path}</d:href>\
         <d:propstat><d:prop>{resource}{etag}</d:prop>\
         <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"
    );
    Answer {
        status: 207,
        headers: vec![("Content-Type", "application/xml; charset=utf-8".to_owned())],
        body: body.into_bytes(),
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        207 => "Multi-Status",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        412 => "Precondition Failed",
        _ => "Status",
    }
}

fn decode(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = bytes
            .get(index + 1..index + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (bytes[index], hex) {
            (b'%', Some(value)) => {
                decoded.push(value);
                index += 3;
            }
            (byte, _) => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}
