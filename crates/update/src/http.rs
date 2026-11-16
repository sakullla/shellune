//! The HTTP side: one `ureq` agent per request, set up like the WebDAV client's.
//!
//! rustls with the system certificate verifier (one TLS stack in the
//! application), the proxy from `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` unless
//! `NO_PROXY` names the host (a loopback host never uses one), and redirects
//! followed: release files are served from a redirect. The proxy is chosen for the
//! first address and kept across its redirects.
//!
//! A download has no limit on its total time: a slow link that keeps delivering
//! is waited for. It fails only when no data arrives for the idle limit
//! ([`DOWNLOAD_IDLE_TIMEOUT`] by default): the agent has no body limit, only a
//! per-read (and per-write) one, which restarts with every read.

use std::time::Duration;

use ureq::http;

use crate::UpdateError;

/// The whole manifest request.
pub(crate) const CHECK_TIMEOUT: Duration = Duration::from_secs(15);

/// Connecting, for a download.
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Waiting for a download's answer to start.
const DOWNLOAD_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// The longest a download waits for its next bytes.
pub(crate) const DOWNLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// What a request is for, which sets its time limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Purpose {
    /// The manifest, within this time.
    Check(Duration),
    /// An update file, failing only after this long without data.
    Download(Duration),
}

/// An agent for a request to `url`.
pub(crate) fn agent_for(url: &str, purpose: Purpose) -> Result<ureq::Agent, UpdateError> {
    let uri: http::Uri = url.parse().map_err(|_| UpdateError::InvalidConfig {
        reason: format!("\"{url}\" is not a valid address"),
    })?;
    match uri.scheme_str() {
        Some("http" | "https") if uri.host().is_some_and(|host| !host.is_empty()) => {}
        _ => {
            return Err(UpdateError::InvalidConfig {
                reason: format!("\"{url}\" is not an http:// or https:// address"),
            });
        }
    }
    let proxy = proxy_for(url, &|name| std::env::var(name).ok())
        .map(|proxy| {
            ureq::Proxy::new(&proxy).map_err(|error| UpdateError::Unreachable {
                reason: format!("the proxy from the environment is not usable: {error}"),
            })
        })
        .transpose()?;
    let builder = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(10)
        .proxy(proxy)
        .user_agent(format!("Shellune/{}", env!("CARGO_PKG_VERSION")))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        );
    Ok(match purpose {
        Purpose::Check(limit) => builder.timeout_global(Some(limit)).build().into(),
        Purpose::Download(idle) => builder
            .timeout_connect(Some(DOWNLOAD_CONNECT_TIMEOUT))
            .timeout_recv_response(Some(DOWNLOAD_RESPONSE_TIMEOUT))
            .timeout_recv_body(None)
            .timeout_per_read(Some(idle))
            .timeout_per_write(Some(idle))
            .build()
            .into(),
    })
}

/// Sorts a transport failure into what the user can act on.
pub(crate) fn classify(error: ureq::Error) -> UpdateError {
    match error {
        ureq::Error::Timeout(_) => UpdateError::Timeout,
        ureq::Error::HostNotFound => UpdateError::Unreachable {
            reason: "the server name could not be found".to_owned(),
        },
        ureq::Error::BadUri(reason) => UpdateError::InvalidConfig { reason },
        ureq::Error::InvalidProxyUrl => UpdateError::Unreachable {
            reason: "the proxy from the environment is not a valid address".to_owned(),
        },
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => UpdateError::Unreachable {
            reason: "the server's redirects could not be followed".to_owned(),
        },
        other => UpdateError::Unreachable {
            reason: other.to_string(),
        },
    }
}

/// The proxy for a request to `url`, from the environment: the scheme's own
/// variable first (`HTTPS_PROXY` for https, `HTTP_PROXY` for http), then
/// `ALL_PROXY`, each in upper and lower case; none when `NO_PROXY` lists the host
/// or the host is the loopback.
///
/// The same rules as the WebDAV client's (`shellune-sync`), which keeps them
/// private.
pub(crate) fn proxy_for(url: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let lookup = |names: &[&str]| {
        names
            .iter()
            .filter_map(|name| env(name))
            .map(|value| value.trim().to_owned())
            .find(|value| !value.is_empty())
    };
    let uri: http::Uri = url.parse().ok()?;
    let host = uri.host()?.trim_start_matches('[').trim_end_matches(']');
    if is_loopback(host) {
        return None;
    }
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

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// Whether a `NO_PROXY` list names `host`: `*`, the host itself, or a domain it
/// is under (`example.com` and `.example.com` both match `dl.example.com`).
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
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn the_scheme_proxy_comes_first_and_no_proxy_and_loopback_bypass() {
        let env = env_of(&[
            ("HTTPS_PROXY", "http://secure:3128"),
            ("http_proxy", "http://plain:3128"),
            ("ALL_PROXY", "socks5://any:1080"),
        ]);
        assert_eq!(
            proxy_for("https://github.com/x", &env).as_deref(),
            Some("http://secure:3128")
        );
        assert_eq!(
            proxy_for("http://example.org/x", &env).as_deref(),
            Some("http://plain:3128")
        );
        let only_all = env_of(&[("all_proxy", "socks5://any:1080")]);
        assert_eq!(
            proxy_for("https://github.com/", &only_all).as_deref(),
            Some("socks5://any:1080")
        );
        assert_eq!(proxy_for("http://127.0.0.1:8080/latest.json", &env), None);
        assert_eq!(proxy_for("http://localhost/latest.json", &env), None);
        assert_eq!(proxy_for("http://[::1]:9/latest.json", &env), None);
        let bypass = env_of(&[("HTTPS_PROXY", "http://p:1"), ("NO_PROXY", ".github.com")]);
        assert_eq!(proxy_for("https://github.com/", &bypass), None);
        assert_eq!(proxy_for("https://api.github.com/", &bypass), None);
        assert_eq!(
            proxy_for("https://notgithub.com/", &bypass).as_deref(),
            Some("http://p:1")
        );
    }

    #[test]
    fn only_http_addresses_make_an_agent() {
        assert!(agent_for("https://github.com/a", Purpose::Check(CHECK_TIMEOUT)).is_ok());
        assert!(matches!(
            agent_for("ftp://example.com/a", Purpose::Check(CHECK_TIMEOUT)),
            Err(UpdateError::InvalidConfig { .. })
        ));
        assert!(matches!(
            agent_for("not a url", Purpose::Download(DOWNLOAD_IDLE_TIMEOUT)),
            Err(UpdateError::InvalidConfig { .. })
        ));
    }
}
