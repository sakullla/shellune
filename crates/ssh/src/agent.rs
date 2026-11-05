//! Reaching the three system agents Shellune supports (R4).
//!
//! The three are different transports to the same protocol, and each one is named
//! when it cannot be used:
//!
//! * SSH_AUTH_SOCK, the Unix socket agent, including macOS;
//! * the Windows OpenSSH named pipe, which is what ssh-agent.exe serves;
//! * Pageant, the PuTTY agent on Windows.
//!
//! A build can only reach the agents of its own platform, so the other two report
//! that they are unavailable here rather than pretending to have tried. Nothing in
//! this module ever holds a private key: the agent signs, and only public identities
//! and signatures cross this boundary.

use russh::keys::agent::client::{AgentClient, AgentStream};

use crate::error::ConnectError;

/// The agent type-restricted to one boxed stream, so one code path can serve all
/// three transports.
pub type DynamicAgent = AgentClient<Box<dyn AgentStream + Send + Unpin>>;

/// Name of the Windows OpenSSH agent pipe.
///
/// ssh-agent.exe creates exactly this name, and ssh-add.exe talks to it, so using it
/// means "the agent Windows already runs" rather than a Shellune specific one.
#[cfg(windows)]
pub const OPENSSH_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

/// The agent a fresh connection attempt uses on this platform.
#[must_use]
pub fn default_kind() -> shellune_core::AgentKind {
    if cfg!(windows) {
        shellune_core::AgentKind::OpenSshPipe
    } else {
        shellune_core::AgentKind::UnixSocket
    }
}

/// Connects to one of the supported agents.
///
/// # Errors
///
/// Returns [ConnectError::AgentUnavailable] with the reason the system gave, which
/// is what separates "no agent is running" from "this build cannot reach that agent
/// on this platform".
pub async fn connect(kind: shellune_core::AgentKind) -> Result<DynamicAgent, ConnectError> {
    match kind {
        shellune_core::AgentKind::UnixSocket => connect_unix_socket().await,
        shellune_core::AgentKind::OpenSshPipe => connect_openssh_pipe().await,
        shellune_core::AgentKind::Pageant => connect_pageant().await,
    }
}

#[cfg(unix)]
async fn connect_unix_socket() -> Result<DynamicAgent, ConnectError> {
    AgentClient::connect_env()
        .await
        .map(AgentClient::dynamic)
        .map_err(|error| ConnectError::AgentUnavailable {
            agent: shellune_core::AgentKind::UnixSocket,
            reason: agent_reason(&error),
        })
}

#[cfg(not(unix))]
async fn connect_unix_socket() -> Result<DynamicAgent, ConnectError> {
    Err(ConnectError::AgentUnavailable {
        agent: shellune_core::AgentKind::UnixSocket,
        reason: "SSH_AUTH_SOCK only exists on Unix builds, and this is not one".to_owned(),
    })
}

#[cfg(windows)]
async fn connect_openssh_pipe() -> Result<DynamicAgent, ConnectError> {
    AgentClient::connect_named_pipe(OPENSSH_AGENT_PIPE)
        .await
        .map(AgentClient::dynamic)
        .map_err(|error| ConnectError::AgentUnavailable {
            agent: shellune_core::AgentKind::OpenSshPipe,
            reason: agent_reason(&error),
        })
}

#[cfg(not(windows))]
async fn connect_openssh_pipe() -> Result<DynamicAgent, ConnectError> {
    Err(ConnectError::AgentUnavailable {
        agent: shellune_core::AgentKind::OpenSshPipe,
        reason: "the OpenSSH agent pipe only exists on Windows".to_owned(),
    })
}

#[cfg(windows)]
async fn connect_pageant() -> Result<DynamicAgent, ConnectError> {
    AgentClient::connect_pageant()
        .await
        .map(AgentClient::dynamic)
        .map_err(|error| ConnectError::AgentUnavailable {
            agent: shellune_core::AgentKind::Pageant,
            reason: agent_reason(&error),
        })
}

#[cfg(not(windows))]
async fn connect_pageant() -> Result<DynamicAgent, ConnectError> {
    Err(ConnectError::AgentUnavailable {
        agent: shellune_core::AgentKind::Pageant,
        reason: "Pageant only exists on Windows".to_owned(),
    })
}

/// Measures one agent: whether it can be reached and how many identities it holds.
///
/// Reachability alone is not enough to tell the user anything useful — an agent that
/// answers with no key behaves exactly like an agent that is not running, from a
/// connection's point of view. This is the call a diagnostic or a test makes to say
/// which of the two it is.
///
/// # Errors
///
/// Returns [ConnectError::AgentUnavailable] when the agent cannot be reached and the
/// same error with the agent's own reason when it cannot be asked for its keys.
pub async fn probe(kind: shellune_core::AgentKind) -> Result<usize, ConnectError> {
    let mut client = connect(kind).await?;
    let identities =
        client
            .request_identities()
            .await
            .map_err(|error| ConnectError::AgentUnavailable {
                agent: kind,
                reason: error.to_string(),
            })?;
    Ok(identities.len())
}

/// The system's own words for why the agent could not be reached.
///
/// The agent protocol carries identities and signatures, never a secret, so its
/// error text is safe to show; it is kept verbatim because "the pipe does not exist"
/// and "the socket path is empty" need different fixes.
fn agent_reason(error: &russh::keys::Error) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shellune_core::AgentKind;

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime")
            .block_on(future)
    }

    #[test]
    fn the_default_agent_follows_the_platform_it_runs_on() {
        let kind = default_kind();
        if cfg!(windows) {
            assert_eq!(kind, AgentKind::OpenSshPipe);
        } else {
            assert_eq!(kind, AgentKind::UnixSocket);
        }
    }

    #[test]
    fn an_agent_that_cannot_be_reached_is_named_with_the_systems_reason() {
        // This test asserts the shape of the answer rather than the answer itself:
        // a machine may really have an agent running, and then connecting is the
        // correct result. What must never happen is a silent failure or a panic.
        for kind in [
            AgentKind::UnixSocket,
            AgentKind::OpenSshPipe,
            AgentKind::Pageant,
        ] {
            let measured = block_on(probe(kind));
            match &measured {
                Ok(identities) => {
                    eprintln!(
                        "measured: the {} holds {identities} identities",
                        kind.label()
                    );
                }
                Err(error) => eprintln!("measured: the {} -> {error}", kind.label()),
            }
            match measured.map(|_| ()) {
                Ok(_) => {}
                Err(error @ ConnectError::AgentUnavailable { agent, .. }) => {
                    assert_eq!(agent, kind);
                    assert!(
                        error.to_string().contains(kind.label()),
                        "the reason must name the agent: {error}"
                    );
                    assert!(
                        !error.next_step().is_empty(),
                        "an unavailable agent needs a next step"
                    );
                }
                Err(other) => panic!("{kind:?} produced an unexpected failure: {other}"),
            }
        }
    }

    #[test]
    fn a_platform_that_cannot_host_an_agent_says_so_instead_of_trying() {
        if cfg!(windows) {
            let error = block_on(connect(AgentKind::UnixSocket))
                .err()
                .expect("a Windows build cannot reach SSH_AUTH_SOCK");
            assert!(error.to_string().contains("SSH_AUTH_SOCK"), "{error}");
        } else {
            let error = block_on(connect(AgentKind::Pageant))
                .err()
                .expect("only Windows runs Pageant");
            assert!(error.to_string().contains("Pageant"), "{error}");
        }
    }
}
