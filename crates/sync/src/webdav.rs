//! A small WebDAV client: just what sync needs from a server.
//!
//! One folder on the server (`remote_dir` under the account's address) holds one
//! file, [`REMOTE_FILE_NAME`]. The client can check that the folder is reachable and
//! writable ([`WebDavClient::test`]), read the file's version ([`WebDavClient::stat`]),
//! read it ([`WebDavClient::get`]) and replace it only when it is still the version
//! that was read ([`WebDavClient::put`] with [`Precondition::IfMatch`]).
//!
//! Requests go through `ureq` with rustls and the system certificate verifier. A
//! proxy named by `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` (and the lower-case
//! forms) is used unless `NO_PROXY` lists the server. Redirects are not followed: a
//! redirected WebDAV write would lose its body or its method, so the answer is
//! reported instead.

use std::fmt;
use std::time::Duration;

use base64::Engine as _;
use shellune_core::SecretString;
use ureq::http;

/// The file sync keeps in the remote folder.
pub const REMOTE_FILE_NAME: &str = "shellune-data.json";

/// The largest remote file read; a data file is far smaller.
const READ_LIMIT: u64 = 64 * 1024 * 1024;

/// How long one request may take in total.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long connecting may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

const PROPFIND_BODY: &str = concat!(
    r#"<?xml version="1.0" encoding="utf-8"?>"#,
    r#"<d:propfind xmlns:d="DAV:"><d:prop>"#,
    r#"<d:getetag/><d:getlastmodified/><d:resourcetype/>"#,
    r#"</d:prop></d:propfind>"#
);

/// Why a WebDAV request did not do what was asked.
///
/// No variant carries the password or the authorization header.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WebDavError {
    /// The address cannot be used as a WebDAV location.
    #[error("the server address is not usable: {reason}")]
    InvalidAddress {
        /// Why.
        reason: String,
    },
    /// The server refused the user name or password (401), or refused access to
    /// the folder (403 on reading it).
    #[error("the server refused the user name or password (HTTP {status})")]
    Auth {
        /// The status the server answered.
        status: u16,
    },
    /// The server could not be reached: no network, a wrong name, a refused or
    /// broken connection, a timeout, or a certificate that is not trusted.
    #[error("the server could not be reached: {reason}")]
    Unreachable {
        /// What failed.
        reason: String,
    },
    /// The folder exists (or could not be made) but the server does not let this
    /// account write to it.
    #[error("the folder \"{path}\" on the server cannot be written (HTTP {status})")]
    NotWritable {
        /// The remote folder.
        path: String,
        /// The status the server answered.
        status: u16,
    },
    /// The file changed on the server after it was read (412): the write was not
    /// made.
    #[error("the file on the server changed while it was being replaced")]
    Conflict,
    /// Any other answer.
    #[error("the server answered {method} with HTTP {status}{}", location_sentence(.location.as_deref()))]
    Status {
        /// The request method.
        method: String,
        /// The status.
        status: u16,
        /// Where a redirect pointed.
        location: Option<String>,
    },
    /// The answer could not be understood.
    #[error("the server's answer could not be understood: {reason}")]
    Protocol {
        /// Why.
        reason: String,
    },
}

fn location_sentence(location: Option<&str>) -> String {
    location.map_or_else(String::new, |location| {
        format!(" (it points to {location}; use that address)")
    })
}

/// What a write requires of the file already on the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Precondition {
    /// Write whatever is there.
    None,
    /// Write only if the file is still this version (`If-Match`).
    IfMatch(String),
    /// Write only if there is no file yet (`If-None-Match: *`).
    IfAbsent,
}

/// The version of the remote file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteFile {
    /// The entity tag, normalized to its quoted form (`"x"` or `W/"x"`); `None`
    /// when the server does not report one.
    pub etag: Option<String>,
}

/// The remote file's content and version.
#[derive(Clone, PartialEq, Eq)]
pub struct Download {
    /// The bytes.
    pub bytes: Vec<u8>,
    /// The entity tag of exactly these bytes, when the server reports one.
    pub etag: Option<String>,
}

impl fmt::Debug for Download {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Download")
            .field("bytes", &self.bytes.len())
            .field("etag", &self.etag)
            .finish()
    }
}

/// The account and folder a client talks to.
#[derive(Clone)]
pub struct WebDavClient {
    agent: ureq::Agent,
    /// The account's address, ending with `/`.
    base: String,
    /// The encoded folder segments under the base.
    segments: Vec<String>,
    /// The folder as the user wrote it, for messages.
    display_dir: String,
    authorization: Option<String>,
}

impl fmt::Debug for WebDavClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebDavClient")
            .field("base", &self.base)
            .field("segments", &self.segments)
            .field("authorization", &self.authorization.as_ref().map(|_| "***"))
            .finish_non_exhaustive()
    }
}

/// One answer, read whole.
struct Reply {
    status: u16,
    etag: Option<String>,
    location: Option<String>,
    body: Vec<u8>,
}

impl WebDavClient {
    /// A client for `remote_dir` under `url`, signing in as `username`.
    ///
    /// `url` is the account's WebDAV address (`http` or `https`); `remote_dir` is a
    /// folder path under it, `/`-separated, and may be empty for the address
    /// itself. With an empty `username` no authorization is sent.
    ///
    /// # Errors
    ///
    /// Returns [`WebDavError::InvalidAddress`] for an address that is not an
    /// `http`/`https` URL, that carries a user name or password, a query or a
    /// fragment, or for a folder with a `.` or `..` segment.
    pub fn new(
        url: &str,
        remote_dir: &str,
        username: &str,
        password: Option<&SecretString>,
    ) -> Result<Self, WebDavError> {
        let base = normalize_base(url)?;
        let segments = folder_segments(remote_dir)?;
        let proxy = proxy_for(&base, &|name| std::env::var(name).ok())
            .map(|proxy| {
                ureq::Proxy::new(&proxy).map_err(|error| WebDavError::InvalidAddress {
                    reason: format!("the proxy from the environment is not usable: {error}"),
                })
            })
            .transpose()?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .allow_non_standard_methods(true)
            .max_redirects(0)
            .timeout_global(Some(REQUEST_TIMEOUT))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .proxy(proxy)
            .user_agent(format!("Shellune/{}", env!("CARGO_PKG_VERSION")))
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .build()
            .into();
        let authorization = (!username.is_empty()).then(|| {
            let password = password.map_or("", SecretString::expose);
            let token =
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
            format!("Basic {token}")
        });
        Ok(Self {
            agent,
            base,
            display_dir: segments
                .iter()
                .map(|segment| percent_decode(segment))
                .collect::<Vec<_>>()
                .join("/"),
            segments,
            authorization,
        })
    }

    /// The address of the remote folder, ending with `/`.
    #[must_use]
    pub fn folder_url(&self) -> String {
        self.collection_url(self.segments.len())
    }

    /// The address of the remote data file.
    #[must_use]
    pub fn file_url(&self) -> String {
        format!("{}{REMOTE_FILE_NAME}", self.folder_url())
    }

    /// Checks that the folder can be reached, signed in to and written: the folder
    /// is looked up (and made, with any missing parents, when it does not exist),
    /// then a probe file is written into it and removed again.
    ///
    /// # Errors
    ///
    /// Returns [`WebDavError::Auth`] for a refused sign-in,
    /// [`WebDavError::Unreachable`] when the server cannot be reached,
    /// [`WebDavError::NotWritable`] when the folder cannot be made or written, and
    /// the other variants for unexpected answers.
    pub fn test(&self) -> Result<(), WebDavError> {
        self.ensure_folder()?;
        let probe = format!(
            "{}.shellune-probe-{}",
            self.folder_url(),
            uuid::Uuid::new_v4().simple()
        );
        let reply = self.send("PUT", &probe, &[], Some(b"probe".to_vec()))?;
        match reply.status {
            200..=299 => {}
            401 => return Err(WebDavError::Auth { status: 401 }),
            403 | 405 | 409 | 423 | 507 => return Err(self.not_writable(reply.status)),
            status => return Err(status_error("PUT", status, reply.location)),
        }
        // A server that let the probe in but will not remove it still holds the
        // data file fine; the leftover is a few bytes.
        let _ = self.send("DELETE", &probe, &[], None);
        Ok(())
    }

    /// Makes the remote folder (and every missing parent) when it does not exist.
    ///
    /// # Errors
    ///
    /// As [`Self::test`].
    pub fn ensure_folder(&self) -> Result<(), WebDavError> {
        let reply = self.propfind(&self.folder_url())?;
        match reply.status {
            207 | 200 => return Ok(()),
            404 | 409 => {}
            401 | 403 => {
                return Err(WebDavError::Auth {
                    status: reply.status,
                });
            }
            status => return Err(status_error("PROPFIND", status, reply.location)),
        }
        for depth in 1..=self.segments.len() {
            let url = self.collection_url(depth);
            let reply = self.send("MKCOL", &url, &[], None)?;
            match reply.status {
                // Made, or already there (405 is what a server answers for an
                // existing collection).
                200..=299 | 405 => {}
                401 => return Err(WebDavError::Auth { status: 401 }),
                403 | 409 | 423 | 507 => return Err(self.not_writable(reply.status)),
                status => return Err(status_error("MKCOL", status, reply.location)),
            }
        }
        Ok(())
    }

    /// The version of the remote file; `None` when there is no file.
    ///
    /// # Errors
    ///
    /// Returns [`WebDavError::Auth`], [`WebDavError::Unreachable`] or
    /// [`WebDavError::Status`].
    pub fn stat(&self) -> Result<Option<RemoteFile>, WebDavError> {
        let reply = self.propfind(&self.file_url())?;
        match reply.status {
            207 | 200 => {
                let text = String::from_utf8_lossy(&reply.body);
                Ok(Some(RemoteFile {
                    etag: element_text(&text, "getetag")
                        .as_deref()
                        .and_then(normalize_etag)
                        .or(reply.etag),
                }))
            }
            404 | 409 => Ok(None),
            401 | 403 => Err(WebDavError::Auth {
                status: reply.status,
            }),
            status => Err(status_error("PROPFIND", status, reply.location)),
        }
    }

    /// The remote file; `None` when there is none.
    ///
    /// # Errors
    ///
    /// Returns [`WebDavError::Auth`], [`WebDavError::Unreachable`] or
    /// [`WebDavError::Status`].
    pub fn get(&self) -> Result<Option<Download>, WebDavError> {
        let reply = self.send("GET", &self.file_url(), &[], None)?;
        match reply.status {
            200 => Ok(Some(Download {
                bytes: reply.body,
                etag: reply.etag,
            })),
            404 | 409 | 410 => Ok(None),
            401 | 403 => Err(WebDavError::Auth {
                status: reply.status,
            }),
            status => Err(status_error("GET", status, reply.location)),
        }
    }

    /// Writes the remote file, making the folder first when the server says it is
    /// missing. Returns the new version's entity tag (from the answer, or looked up
    /// when the answer has none).
    ///
    /// # Errors
    ///
    /// Returns [`WebDavError::Conflict`] when the precondition failed (the file
    /// changed or appeared), [`WebDavError::NotWritable`] when the server refused
    /// the write, and the errors of [`Self::stat`].
    pub fn put(
        &self,
        bytes: &[u8],
        precondition: &Precondition,
    ) -> Result<Option<String>, WebDavError> {
        let mut headers = vec![("Content-Type", "application/json".to_owned())];
        match precondition {
            Precondition::None => {}
            Precondition::IfMatch(etag) => headers.push(("If-Match", etag.clone())),
            Precondition::IfAbsent => headers.push(("If-None-Match", "*".to_owned())),
        }
        let url = self.file_url();
        let mut reply = self.send("PUT", &url, &headers, Some(bytes.to_vec()))?;
        if reply.status == 409 {
            // The folder is missing (removed on the server, or never made).
            self.ensure_folder()?;
            reply = self.send("PUT", &url, &headers, Some(bytes.to_vec()))?;
        }
        match reply.status {
            200..=299 => match reply.etag {
                Some(etag) => Ok(Some(etag)),
                None => Ok(self.stat()?.and_then(|file| file.etag)),
            },
            412 => Err(WebDavError::Conflict),
            401 => Err(WebDavError::Auth { status: 401 }),
            403 | 405 | 409 | 423 | 507 => Err(self.not_writable(reply.status)),
            status => Err(status_error("PUT", status, reply.location)),
        }
    }

    fn not_writable(&self, status: u16) -> WebDavError {
        WebDavError::NotWritable {
            path: if self.display_dir.is_empty() {
                "/".to_owned()
            } else {
                self.display_dir.clone()
            },
            status,
        }
    }

    fn collection_url(&self, depth: usize) -> String {
        let mut url = self.base.clone();
        for segment in &self.segments[..depth] {
            url.push_str(segment);
            url.push('/');
        }
        url
    }

    fn propfind(&self, url: &str) -> Result<Reply, WebDavError> {
        self.send(
            "PROPFIND",
            url,
            &[
                ("Depth", "0".to_owned()),
                ("Content-Type", "application/xml; charset=utf-8".to_owned()),
            ],
            Some(PROPFIND_BODY.as_bytes().to_vec()),
        )
    }

    fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, String)],
        body: Option<Vec<u8>>,
    ) -> Result<Reply, WebDavError> {
        let method_value =
            http::Method::from_bytes(method.as_bytes()).map_err(|error| WebDavError::Protocol {
                reason: error.to_string(),
            })?;
        let mut builder = http::Request::builder().method(method_value).uri(url);
        if let Some(authorization) = &self.authorization {
            builder = builder.header("Authorization", authorization);
        }
        for (name, value) in headers {
            builder = builder.header(*name, value);
        }
        let invalid = |error: http::Error| WebDavError::InvalidAddress {
            reason: error.to_string(),
        };
        let outcome = match body {
            Some(bytes) => self.agent.run(builder.body(bytes).map_err(invalid)?),
            None => self.agent.run(builder.body(()).map_err(invalid)?),
        };
        let mut response = outcome.map_err(classify)?;
        let status = response.status().as_u16();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let etag = header("etag").as_deref().and_then(normalize_etag);
        let location = header("location");
        let body = response
            .body_mut()
            .with_config()
            .limit(READ_LIMIT)
            .read_to_vec()
            .map_err(classify)?;
        Ok(Reply {
            status,
            etag,
            location,
            body,
        })
    }
}

fn status_error(method: &str, status: u16, location: Option<String>) -> WebDavError {
    WebDavError::Status {
        method: method.to_owned(),
        status,
        location: location.filter(|_| (300..400).contains(&status)),
    }
}

/// Sorts a transport failure into what the user can act on.
fn classify(error: ureq::Error) -> WebDavError {
    match error {
        ureq::Error::BadUri(reason) => WebDavError::InvalidAddress { reason },
        ureq::Error::Http(error) => WebDavError::InvalidAddress {
            reason: error.to_string(),
        },
        ureq::Error::InvalidProxyUrl => WebDavError::InvalidAddress {
            reason: "the proxy from the environment is not a valid address".to_owned(),
        },
        ureq::Error::BodyExceedsLimit(limit) => WebDavError::Protocol {
            reason: format!("the file on the server is larger than {limit} bytes"),
        },
        ureq::Error::HostNotFound => WebDavError::Unreachable {
            reason: "the server name could not be found".to_owned(),
        },
        ureq::Error::Timeout(_) => WebDavError::Unreachable {
            reason: "the server did not answer in time".to_owned(),
        },
        other => WebDavError::Unreachable {
            reason: other.to_string(),
        },
    }
}

/// Characters a path segment keeps as they are (RFC 3986 `pchar` without `%`).
fn is_path_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@".contains(&byte)
}

fn percent_encode(text: &str, keep: impl Fn(u8) -> bool) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if keep(byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = bytes
            .get(index + 1..index + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        if bytes[index] == b'%'
            && let Some(value) = hex
        {
            decoded.push(value);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// The account address with a trailing `/`, characters a URL cannot hold encoded.
fn normalize_base(url: &str) -> Result<String, WebDavError> {
    let invalid = |reason: &str| WebDavError::InvalidAddress {
        reason: reason.to_owned(),
    };
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(invalid("no address is set"));
    }
    if trimmed.contains('#') || trimmed.contains('?') {
        return Err(invalid(
            "the address may not contain a query (?) or a fragment (#)",
        ));
    }
    // Spaces and non-ASCII characters are written as typed; a `%` the user already
    // wrote is kept as an escape.
    let encoded = percent_encode(trimmed, |byte| byte.is_ascii_graphic());
    let uri: http::Uri = encoded
        .parse()
        .map_err(|_| invalid("the address is not a valid URL"))?;
    match uri.scheme_str() {
        Some("http" | "https") => {}
        _ => return Err(invalid("the address must start with http:// or https://")),
    }
    let Some(authority) = uri.authority() else {
        return Err(invalid("the address has no server name"));
    };
    if authority.as_str().contains('@') {
        return Err(invalid(
            "put the user name and password in their own fields, not in the address",
        ));
    }
    if authority.host().is_empty() {
        return Err(invalid("the address has no server name"));
    }
    let mut base = encoded;
    if !base.ends_with('/') {
        base.push('/');
    }
    Ok(base)
}

/// The folder's segments, each percent-encoded.
fn folder_segments(remote_dir: &str) -> Result<Vec<String>, WebDavError> {
    let mut segments = Vec::new();
    for segment in remote_dir.split(['/', '\\']) {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        if segment == "." || segment == ".." {
            return Err(WebDavError::InvalidAddress {
                reason: "the remote folder may not contain \".\" or \"..\"".to_owned(),
            });
        }
        if segment.chars().any(char::is_control) {
            return Err(WebDavError::InvalidAddress {
                reason: "the remote folder contains a control character".to_owned(),
            });
        }
        segments.push(percent_encode(segment, is_path_char));
    }
    Ok(segments)
}

/// An entity tag in its quoted form: `"x"` for a strong one, `W/"x"` for a weak
/// one. A server that leaves the quotes out (some do in PROPFIND) gets them added,
/// so the PROPFIND and GET forms compare equal.
#[must_use]
pub fn normalize_etag(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let (weak, tag) = match raw.strip_prefix("W/").or_else(|| raw.strip_prefix("w/")) {
        Some(rest) => (true, rest.trim()),
        None => (false, raw),
    };
    let tag = tag.trim_matches('"');
    if tag.is_empty() {
        return None;
    }
    Some(if weak {
        format!("W/\"{tag}\"")
    } else {
        format!("\"{tag}\"")
    })
}

/// Whether a tag can be sent in `If-Match` (only strong tags ever match there).
#[must_use]
pub fn is_strong_etag(etag: &str) -> bool {
    !etag.starts_with("W/")
}

/// The text of the first element whose local name is `name`, unescaped, in a
/// PROPFIND answer. Namespace prefixes are ignored; an empty element reads as
/// `None`.
fn element_text(xml: &str, name: &str) -> Option<String> {
    let mut rest = xml;
    while let Some(start) = rest.find('<') {
        let after = &rest[start + 1..];
        let end = after.find('>')?;
        let tag = &after[..end];
        rest = &after[end + 1..];
        if tag.starts_with('/') || tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        let self_closing = tag.ends_with('/');
        let tag_name = tag
            .trim_end_matches('/')
            .split(|c: char| c.is_whitespace())
            .next()
            .unwrap_or("");
        let local = tag_name.rsplit(':').next().unwrap_or(tag_name);
        if local != name {
            continue;
        }
        if self_closing {
            return None;
        }
        let text_end = rest.find('<').unwrap_or(rest.len());
        let text = unescape_xml(rest[..text_end].trim());
        return (!text.is_empty()).then_some(text);
    }
    None
}

fn unescape_xml(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#34;", "\"")
        .replace("&#x22;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The proxy for a request to `url`, from the environment: the scheme's own
/// variable first (`HTTPS_PROXY` for https, `HTTP_PROXY` for http), then
/// `ALL_PROXY`, each in upper and lower case; none when `NO_PROXY` lists the host.
fn proxy_for(url: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let lookup = |names: &[&str]| {
        names
            .iter()
            .filter_map(|name| env(name))
            .map(|value| value.trim().to_owned())
            .find(|value| !value.is_empty())
    };
    let uri: http::Uri = url.parse().ok()?;
    let host = uri.host()?.trim_start_matches('[').trim_end_matches(']');
    if let Some(no_proxy) = lookup(&["NO_PROXY", "no_proxy"])
        && bypasses(&no_proxy, host)
    {
        return None;
    }
    let scheme_names: &[&str] = if uri.scheme_str() == Some("https") {
        &["HTTPS_PROXY", "https_proxy"]
    } else {
        &["HTTP_PROXY", "http_proxy"]
    };
    lookup(scheme_names).or_else(|| lookup(&["ALL_PROXY", "all_proxy"]))
}

/// Whether a `NO_PROXY` list names `host`: `*`, the host itself, or a domain it
/// is under (`example.com` and `.example.com` both match `dav.example.com`).
fn bypasses(no_proxy: &str, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    no_proxy
        .split([',', ' '])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .any(|entry| {
            if entry == "*" {
                return true;
            }
            let entry = entry.to_ascii_lowercase();
            // A port in the entry is not compared.
            let entry = entry
                .rsplit_once(':')
                .filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit()))
                .map_or(entry.as_str(), |(name, _)| name);
            let domain = entry.trim_start_matches('.');
            host == domain || host.ends_with(&format!(".{domain}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn the_scheme_proxy_comes_before_all_proxy_and_no_proxy_wins() {
        let env = env_of(&[
            ("HTTPS_PROXY", "http://secure:8080"),
            ("http_proxy", "http://plain:3128"),
            ("ALL_PROXY", "socks5://any:1080"),
        ]);
        assert_eq!(
            proxy_for("https://dav.example.com/", &env).as_deref(),
            Some("http://secure:8080")
        );
        assert_eq!(
            proxy_for("http://dav.example.com/", &env).as_deref(),
            Some("http://plain:3128")
        );
        let only_all = env_of(&[("all_proxy", "socks5://any:1080")]);
        assert_eq!(
            proxy_for("https://dav.example.com/", &only_all).as_deref(),
            Some("socks5://any:1080")
        );
        assert_eq!(proxy_for("https://dav.example.com/", &env_of(&[])), None);

        let bypass = env_of(&[
            ("HTTPS_PROXY", "http://secure:8080"),
            ("NO_PROXY", "localhost, .example.com,10.0.0.1:443"),
        ]);
        assert_eq!(proxy_for("https://dav.example.com/x/", &bypass), None);
        assert_eq!(proxy_for("https://example.com/", &bypass), None);
        assert_eq!(proxy_for("https://10.0.0.1/", &bypass), None);
        assert_eq!(
            proxy_for("https://notexample.com/", &bypass).as_deref(),
            Some("http://secure:8080")
        );
        let everything = env_of(&[("HTTPS_PROXY", "http://p:1"), ("no_proxy", "*")]);
        assert_eq!(proxy_for("https://dav.example.com/", &everything), None);
    }

    #[test]
    fn entity_tags_are_compared_in_their_quoted_form() {
        assert_eq!(normalize_etag("\"abc\"").as_deref(), Some("\"abc\""));
        assert_eq!(normalize_etag(" abc ").as_deref(), Some("\"abc\""));
        assert_eq!(normalize_etag("W/\"abc\"").as_deref(), Some("W/\"abc\""));
        assert_eq!(normalize_etag("\"\""), None);
        assert_eq!(normalize_etag(""), None);
        assert!(is_strong_etag("\"abc\""));
        assert!(!is_strong_etag("W/\"abc\""));
    }

    #[test]
    fn the_entity_tag_is_read_from_a_propfind_answer_whatever_the_prefix() {
        let answer = r#"<?xml version="1.0"?>
            <D:multistatus xmlns:D="DAV:"><D:response><D:href>/dav/x</D:href>
            <D:propstat><D:prop><D:getetag>&quot;e42&quot;</D:getetag>
            <D:resourcetype/></D:prop></D:propstat></D:response></D:multistatus>"#;
        assert_eq!(element_text(answer, "getetag").as_deref(), Some("\"e42\""));
        let plain = "<multistatus><response><propstat><prop><getetag>abc</getetag>";
        assert_eq!(element_text(plain, "getetag").as_deref(), Some("abc"));
        let missing = "<d:multistatus><d:prop><d:getetag/></d:prop></d:multistatus>";
        assert_eq!(element_text(missing, "getetag"), None);
    }

    #[test]
    fn addresses_and_folders_are_checked_and_encoded() {
        let client = WebDavClient::new(
            "https://dav.example.com/remote.php/dav/files/me",
            "/Shellune data/云端/",
            "me",
            None,
        )
        .expect("usable");
        assert_eq!(
            client.folder_url(),
            "https://dav.example.com/remote.php/dav/files/me/Shellune%20data/%E4%BA%91%E7%AB%AF/"
        );
        assert_eq!(
            client.file_url(),
            "https://dav.example.com/remote.php/dav/files/me/Shellune%20data/%E4%BA%91%E7%AB%AF/shellune-data.json"
        );
        assert_eq!(client.display_dir, "Shellune data/云端");
        let root = WebDavClient::new("http://nas.local:5005/", "", "", None).expect("usable");
        assert_eq!(root.file_url(), "http://nas.local:5005/shellune-data.json");

        for (url, dir) in [
            ("ftp://dav.example.com/", "x"),
            ("dav.example.com", "x"),
            ("https://me:secret@dav.example.com/", "x"),
            ("https://dav.example.com/?a=b", "x"),
            ("", "x"),
            ("https://dav.example.com/", "a/../b"),
        ] {
            assert!(
                matches!(
                    WebDavClient::new(url, dir, "me", None),
                    Err(WebDavError::InvalidAddress { .. })
                ),
                "{url} {dir}"
            );
        }
    }

    #[test]
    fn the_debug_form_does_not_show_the_password() {
        let client = WebDavClient::new(
            "https://dav.example.com/",
            "x",
            "me",
            Some(&SecretString::new("hunter2")),
        )
        .expect("usable");
        let shown = format!("{client:?}");
        assert!(!shown.contains("hunter2"));
        assert!(!shown.contains("aHVudGVyMg"), "{shown}");
        assert!(!shown.contains("Basic"), "{shown}");
    }
}
