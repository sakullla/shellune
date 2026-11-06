//! Browsing directories over the connection the terminal already authenticated (R9).
//!
//! The file view must not ask for a password a second time and must not disturb the
//! shell, so the SFTP work runs on the session's own connection:
//!
//! * the shell loop owns the connection handle, so it is the only place that can
//!   open a channel; it answers a request by opening an SFTP subsystem channel and
//!   handing it over;
//! * the browser then runs in its own task, which owns the SFTP session and the
//!   requests, so a slow directory read never stops the terminal from reading or
//!   writing;
//! * a failure is reported as the answer to the request that caused it, with the
//!   kind of failure the user can act on. Nothing here changes the state of the
//!   terminal: an SFTP channel that could not be opened says nothing about whether
//!   the shell is still alive.
//!
//! Requests and answers are the domain values from shellune-core, which is what lets
//! the browser's state machine be tested without a server at all.

use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::{Config, SftpSession};
use russh_sftp::protocol::{FileType, StatusCode};
use shellune_core::{
    BrowseFailure, BrowseRequest, BrowseResponse, DirEntry, DirectoryListing, FileKind,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

use crate::session::Command;

/// The subsystem name a directory listing needs.
pub const SFTP_SUBSYSTEM: &str = "sftp";

/// How long one SFTP request may take before the browser gives up on it.
///
/// It also bounds the opening handshake, which is why a server that has no SFTP
/// subsystem at all answers "no SFTP" within this time: russh reports the subsystem
/// request as sent without waiting for the server's decision, so the refusal is only
/// visible as a handshake that never arrives.
pub const REQUEST_TIMEOUT_SECS: u64 = 10;

/// Why the shell loop could not hand over an SFTP channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChannelFailure {
    /// The server accepted the connection but has no SFTP subsystem.
    Unavailable {
        /// What the server or the protocol said.
        reason: String,
    },
    /// The connection could not carry the request at all.
    Lost {
        /// What the transport said.
        reason: String,
    },
}

/// The connection thread's side of the file browser.
///
/// The session builds it before the connection exists, so the browser's channels are
/// in place whether or not a connection ever needs them.
pub(crate) struct Wiring {
    /// Requests the browser made.
    pub requests: UnboundedReceiver<BrowseRequest>,
    /// Where the answers go back to the browser.
    pub responses: UnboundedSender<BrowseResponse>,
}

/// The application's side of one browsable connection.
///
/// It is taken from the session before the terminal owns it, so the file view keeps
/// a way to ask for directories for as long as the connection lives, and a closed
/// link answers false instead of queueing a request nobody will serve.
pub struct FileLink {
    requests: UnboundedSender<BrowseRequest>,
    responses: UnboundedReceiver<BrowseResponse>,
    closed: bool,
}

impl FileLink {
    /// A link over the two channels the session created for it.
    pub(crate) fn new(
        requests: UnboundedSender<BrowseRequest>,
        responses: UnboundedReceiver<BrowseResponse>,
    ) -> Self {
        Self {
            requests,
            responses,
            closed: false,
        }
    }

    /// Asks for something, returning whether the request reached the connection.
    ///
    /// A request that could not be delivered is not silently dropped: the caller
    /// reports the connection as gone instead of waiting for an answer that will
    /// never come.
    pub fn send(&mut self, request: BrowseRequest) -> bool {
        if self.closed {
            return false;
        }
        if self.requests.send(request).is_err() {
            self.closed = true;
            return false;
        }
        true
    }

    /// Every answer that arrived since the last call.
    pub fn drain(&mut self) -> Vec<BrowseResponse> {
        let mut responses = Vec::new();
        while let Ok(response) = self.responses.try_recv() {
            responses.push(response);
        }
        responses
    }

    /// Stops asking; used when the file view or its session is closed.
    pub fn close(&mut self) {
        self.closed = true;
    }

    /// Whether the link stopped accepting requests.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

/// Serves browse requests until the connection ends.
///
/// The loop is deliberately separate from the terminal: while a directory is being
/// read the shell keeps reading and writing through its own channel, because both
/// are independent channels of the same connection.
pub(crate) async fn serve(wiring: Wiring, commands: UnboundedSender<Command>) {
    let Wiring {
        mut requests,
        responses,
    } = wiring;
    let mut session: Option<SftpSession> = None;
    while let Some(request) = requests.recv().await {
        let number = request.request();
        let response = match answer(&mut session, &commands, request).await {
            Ok(response) => response,
            Err(failure) => BrowseResponse::Failed {
                request: number,
                failure,
            },
        };
        if responses.send(response).is_err() {
            break;
        }
    }
    if let Some(session) = session {
        let _ = session.close().await;
    }
}

/// Answers one request, opening the SFTP session on first use.
async fn answer(
    session: &mut Option<SftpSession>,
    commands: &UnboundedSender<Command>,
    request: BrowseRequest,
) -> Result<BrowseResponse, BrowseFailure> {
    let number = request.request();
    let requested = request.path().map(str::to_owned);

    let outcome = {
        let sftp = open_session(session, commands).await?;
        match requested {
            Some(path) => read_directory(sftp, path).await,
            // The login directory is asked of the server rather than guessed from
            // the user name: only the server knows where the account starts.
            None => match canonicalize(sftp, ".").await {
                Ok(home) => read_directory(sftp, home).await,
                Err(failure) => Err(failure),
            },
        }
    };

    // A session that could not be spoken to is dropped, so the next request opens a
    // fresh channel instead of repeating the same dead one. A refusal about the path
    // keeps the session: the connection is fine, the directory is not.
    if outcome
        .as_ref()
        .err()
        .is_some_and(BrowseFailure::is_connection_lost)
    {
        *session = None;
    }

    let listing = outcome?;
    Ok(BrowseResponse::Listed {
        request: number,
        path: listing.path().to_owned(),
        entries: listing.entries().to_vec(),
    })
}

/// The SFTP session of this connection, opened on first use.
async fn open_session<'a>(
    slot: &'a mut Option<SftpSession>,
    commands: &UnboundedSender<Command>,
) -> Result<&'a mut SftpSession, BrowseFailure> {
    if slot.is_none() {
        let (reply, answer) = oneshot::channel();
        if commands.send(Command::Sftp(reply)).is_err() {
            return Err(BrowseFailure::ConnectionLost {
                reason: "the SSH connection is no longer open".to_owned(),
            });
        }
        let channel = answer
            .await
            .map_err(|_| BrowseFailure::ConnectionLost {
                reason: "the SSH connection ended before the channel was opened".to_owned(),
            })?
            .map_err(|failure| match failure {
                ChannelFailure::Unavailable { reason } => BrowseFailure::Unavailable { reason },
                ChannelFailure::Lost { reason } => BrowseFailure::ConnectionLost { reason },
            })?;
        let config = Config {
            request_timeout_secs: REQUEST_TIMEOUT_SECS,
            ..Config::default()
        };
        // A server without the subsystem accepts the channel request and then says
        // nothing, so the handshake that never answers is what "no SFTP here" looks
        // like from this side. The reason says which of the two happened.
        let opened = SftpSession::new_with_config(channel.into_stream(), config)
            .await
            .map_err(|error| BrowseFailure::Unavailable {
                reason: match error {
                    SftpError::Timeout => "the server did not answer the sftp handshake".to_owned(),
                    other => other.to_string(),
                },
            })?;
        *slot = Some(opened);
    }
    slot.as_mut().ok_or(BrowseFailure::Unavailable {
        reason: "the sftp session could not be opened".to_owned(),
    })
}

/// Resolves one path on the server.
async fn canonicalize(session: &SftpSession, path: &str) -> Result<String, BrowseFailure> {
    session
        .canonicalize(path)
        .await
        .map_err(|error| describe(&error, path))
}

/// Reads one directory, classifying what the server did.
async fn read_directory(
    session: &mut SftpSession,
    path: String,
) -> Result<DirectoryListing, BrowseFailure> {
    let entries = match session.read_dir(&path).await {
        Ok(read_dir) => read_dir
            .filter_map(|entry| {
                let name = entry.file_name();
                let metadata = entry.metadata();
                let kind = match metadata.file_type() {
                    FileType::Dir => FileKind::Directory,
                    FileType::File => FileKind::File,
                    FileType::Symlink => FileKind::Symlink,
                    FileType::Other => FileKind::Other,
                };
                let size = match kind {
                    FileKind::Directory => None,
                    _ => metadata.size,
                };
                DirEntry::new(name, kind, size)
            })
            .collect(),
        Err(error) => {
            // A path that exists but is a file is answered by OpenSSH with either a
            // plain failure or "no such file", because a file cannot be opened as a
            // directory. The entry is therefore looked at before the failure is
            // reported: "not a directory" is what the user did, and it says more than
            // the code the server chose.
            let failure = describe(&error, &path);
            if matches!(
                failure,
                BrowseFailure::Refused { .. } | BrowseFailure::NotFound { .. }
            ) && let Ok(metadata) = session.metadata(&path).await
                && !metadata.is_dir()
            {
                return Err(BrowseFailure::NotADirectory { path: path.clone() });
            }
            return Err(failure);
        }
    };
    Ok(DirectoryListing::new(path, entries))
}

/// The failure one SFTP error describes, named for the path it happened at.
fn describe(error: &SftpError, path: &str) -> BrowseFailure {
    match error {
        SftpError::Status(status) => match status.status_code {
            StatusCode::NoSuchFile => BrowseFailure::NotFound {
                path: path.to_owned(),
            },
            StatusCode::PermissionDenied => BrowseFailure::PermissionDenied {
                path: path.to_owned(),
            },
            StatusCode::NoConnection | StatusCode::ConnectionLost => {
                BrowseFailure::ConnectionLost {
                    reason: status.error_message.clone(),
                }
            }
            other => BrowseFailure::Refused {
                reason: format!("{} ({other})", status.error_message),
            },
        },
        SftpError::Timeout => BrowseFailure::ConnectionLost {
            reason: "the server did not answer the SFTP request".to_owned(),
        },
        SftpError::IO(reason) | SftpError::Limited(reason) => BrowseFailure::ConnectionLost {
            reason: reason.clone(),
        },
        other => BrowseFailure::Refused {
            reason: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_path_is_reported_as_the_path_the_user_asked_for() {
        let status = russh_sftp::protocol::Status {
            id: 0,
            status_code: StatusCode::NoSuchFile,
            error_message: "No such file".to_owned(),
            language_tag: String::new(),
        };
        let failure = describe(&SftpError::Status(status), "/home/deploy/文档");
        assert_eq!(
            failure,
            BrowseFailure::NotFound {
                path: "/home/deploy/文档".to_owned()
            }
        );
        assert!(failure.to_string().contains("/home/deploy/文档"));
    }

    #[test]
    fn a_refused_directory_is_not_a_lost_connection() {
        let status = russh_sftp::protocol::Status {
            id: 0,
            status_code: StatusCode::PermissionDenied,
            error_message: "Permission denied".to_owned(),
            language_tag: String::new(),
        };
        let failure = describe(&SftpError::Status(status), "/root");
        assert!(!failure.is_connection_lost());
        assert_eq!(
            failure.path(),
            Some("/root"),
            "the failure names the directory"
        );
    }

    #[test]
    fn a_broken_transport_is_a_lost_connection() {
        let failure = describe(&SftpError::IO("channel closed".to_owned()), "/home");
        assert!(failure.is_connection_lost());
        assert!(!failure.to_string().is_empty());
    }

    #[test]
    fn a_link_stops_accepting_requests_once_it_is_closed() {
        let (requests, _request_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_responses_tx, responses) = tokio::sync::mpsc::unbounded_channel();
        let mut link = FileLink::new(requests, responses);
        assert!(link.send(BrowseRequest::Home { request: 1 }));
        link.close();
        assert!(link.is_closed());
        assert!(
            !link.send(BrowseRequest::Home { request: 2 }),
            "a closed link must not promise an answer"
        );
        assert!(link.drain().is_empty());
    }
}
