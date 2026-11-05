//! One remote session: host trust, one authentication method and a real remote PTY.
//!
//! The shape of this module follows the local PTY in shellune-terminal on purpose:
//! a background thread owns the async runtime, the shell polls for events and sends
//! commands, and nothing in the window ever awaits the network. That is what lets
//! the same terminal pane drive a remote shell without knowing that SSH exists.
//!
//! Three rules hold:
//!
//! * an unknown identity is only used after the user answers the question, so
//!   check_server_key blocks on a decision instead of guessing;
//! * a changed identity ends the attempt and is never turned into an "accept the new
//!   key" flow, because the trust record must not be overwritten by a connection
//!   that was not deliberately re-authorised;
//! * exactly one authentication method is tried. A failure is reported as that
//!   method failing, and the user chooses another one, which is why nothing here
//!   loops over methods behind the user's back.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg};
use russh::{ChannelMsg, Disconnect};
use shellune_core::{AgentKind, AuthMethod};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::agent;
use crate::auth::load_private_key;
use crate::error::ConnectError;
use crate::hostkey::{HostKeyPrompt, KnownHostKey, PresentedKey};

/// Value of the TERM variable the remote shell is told about.
///
/// The same value the local PTY uses, so a program that looks right locally looks
/// the same over SSH.
pub const TERM_VALUE: &str = "xterm-256color";

/// How long a connection attempt may take before it is reported as a timeout.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Keepalive interval, so a silently dropped network is noticed.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// Size of a remote pseudo terminal, in characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PtySize {
    /// Columns.
    pub cols: u32,
    /// Rows.
    pub rows: u32,
}

impl PtySize {
    /// Largest size a server is asked for.
    pub const MAX: u32 = 1000;

    /// A size, kept inside the range a terminal server accepts.
    #[must_use]
    pub fn new(cols: u32, rows: u32) -> Self {
        Self {
            cols: cols.clamp(1, Self::MAX),
            rows: rows.clamp(1, Self::MAX),
        }
    }
}

impl Default for PtySize {
    fn default() -> Self {
        Self { cols: 80, rows: 24 }
    }
}

/// What the user answered when the shell asked about an unknown identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustAnswer {
    /// The fingerprint was compared and accepted.
    Accept,
    /// The identity was refused, and nothing is stored.
    Reject,
}

/// Everything one attempt needs.
pub struct ConnectRequest {
    /// Host name or address, as saved.
    pub host: String,
    /// TCP port.
    pub port: u16,
    /// Login user.
    pub user: String,
    /// The one method this attempt uses.
    pub auth: AuthMethod,
    /// Initial terminal size.
    pub size: PtySize,
    /// What the trust records say about this host before the attempt.
    pub known: KnownHostKey,
}

impl ConnectRequest {
    /// Builds a request.
    #[must_use]
    pub fn new(
        host: impl Into<String>,
        port: u16,
        user: impl Into<String>,
        auth: AuthMethod,
        size: PtySize,
        known: KnownHostKey,
    ) -> Self {
        Self {
            host: host.into(),
            port,
            user: user.into(),
            auth,
            size,
            known,
        }
    }

    /// The address the attempt dials, in the form error messages use.
    #[must_use]
    pub fn target(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }
}

/// Something that happened on the way to, or inside, a remote session.
#[derive(Debug)]
pub enum RemoteEvent {
    /// The server identity is unknown and the user has to decide.
    HostKeyPrompt(HostKeyPrompt),
    /// Authentication succeeded and the remote shell is running.
    Connected,
    /// Bytes the remote shell produced.
    Data(Vec<u8>),
    /// The remote shell ended.
    Exited {
        /// Exit status, when the server reported one.
        code: Option<u32>,
    },
    /// The attempt failed, or a live session stopped.
    Failed(ConnectError),
}

/// What the window tells the connection thread to do.
enum Command {
    Write(Vec<u8>),
    Resize(PtySize),
    Close,
}

/// A remote session, from the first handshake to its last byte.
pub struct RemoteSession {
    commands: UnboundedSender<Command>,
    decisions: UnboundedSender<TrustAnswer>,
    events: UnboundedReceiver<RemoteEvent>,
    size: PtySize,
    closed: bool,
}

impl RemoteSession {
    /// Starts connecting on a background thread.
    ///
    /// The call returns immediately: the attempt reports through
    /// [RemoteSession::drain], so the window never waits for the network.
    #[must_use]
    pub fn connect(request: ConnectRequest) -> Self {
        let (commands, command_rx) = unbounded_channel();
        let (events, event_rx) = unbounded_channel();
        let (decisions, decision_rx) = unbounded_channel();
        let size = request.size;

        let failure_sender = events.clone();
        thread::Builder::new()
            .name("shellune-ssh".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build();
                let Ok(runtime) = runtime else {
                    let _ = failure_sender.send(RemoteEvent::Failed(ConnectError::Unreachable {
                        reason: "the SSH runtime could not be started".to_owned(),
                    }));
                    return;
                };
                runtime.block_on(run(request, command_rx, events, decision_rx));
            })
            .expect("the SSH thread can be created");

        Self {
            commands,
            decisions,
            events: event_rx,
            size,
            closed: false,
        }
    }

    /// Answers the question about an unknown identity.
    ///
    /// Returns whether the answer could be delivered; a question that is already
    /// answered, or an attempt that ended, reports false instead of pretending the
    /// decision still matters.
    pub fn answer_host_key(&mut self, accept: bool) -> bool {
        let answer = if accept {
            TrustAnswer::Accept
        } else {
            TrustAnswer::Reject
        };
        self.decisions.send(answer).is_ok()
    }

    /// Sends bytes to the remote shell.
    ///
    /// # Errors
    ///
    /// Returns the reason when the connection thread is gone, which is the moment the
    /// caller must stop believing input still reaches the shell.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.commands
            .send(Command::Write(bytes.to_vec()))
            .map_err(|_| "the SSH connection is no longer open".to_owned())
    }

    /// Tells the remote shell the window changed size.
    ///
    /// # Errors
    ///
    /// Returns the reason when the connection thread is gone.
    pub fn resize(&mut self, size: PtySize) -> Result<(), String> {
        let size = PtySize::new(size.cols, size.rows);
        if size == self.size {
            return Ok(());
        }
        self.size = size;
        self.commands
            .send(Command::Resize(size))
            .map_err(|_| "the SSH connection is no longer open".to_owned())
    }

    /// Every event that arrived since the last call.
    pub fn drain(&mut self) -> Vec<RemoteEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }

    /// The size the remote terminal was last told about.
    #[must_use]
    pub fn size(&self) -> PtySize {
        self.size
    }

    /// Ends the session.
    ///
    /// Best effort on purpose: a session that already ended must not turn a clean
    /// close into an error the user has to read.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let _ = self.commands.send(Command::Close);
    }

    /// Whether [RemoteSession::close] was called.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

impl Drop for RemoteSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// Runs one attempt to completion, reporting everything through the event channel.
async fn run(
    request: ConnectRequest,
    commands: UnboundedReceiver<Command>,
    events: UnboundedSender<RemoteEvent>,
    decisions: UnboundedReceiver<TrustAnswer>,
) {
    if let Err(error) = connect_and_shell(request, commands, &events, decisions).await {
        let _ = events.send(RemoteEvent::Failed(error));
    }
}

async fn connect_and_shell(
    request: ConnectRequest,
    mut commands: UnboundedReceiver<Command>,
    events: &UnboundedSender<RemoteEvent>,
    decisions: UnboundedReceiver<TrustAnswer>,
) -> Result<(), ConnectError> {
    let target = request.target();
    let material = Material::resolve(request.auth).await?;

    let config = Arc::new(client::Config {
        nodelay: true,
        keepalive_interval: Some(KEEPALIVE_INTERVAL),
        ..Default::default()
    });
    let recorded = Arc::new(Mutex::new(None::<ConnectError>));
    let handler = ClientHandler {
        host: request.host.clone(),
        port: request.port,
        known: request.known.clone(),
        events: events.clone(),
        decisions,
        recorded: Arc::clone(&recorded),
    };

    let connecting = client::connect(config, (request.host.as_str(), request.port), handler);
    let mut handle = match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
        Err(_) => {
            return Err(ConnectError::TimedOut { target });
        }
        Ok(Err(error)) => {
            // The handler knows more than the transport error does: it saw the
            // identity that was refused or that changed, so its verdict wins.
            let recorded = recorded.lock().ok().and_then(|slot| slot.clone());
            return Err(recorded.unwrap_or(error));
        }
        Ok(Ok(handle)) => handle,
    };

    authenticate(&mut handle, &request.user, material).await?;

    let channel =
        handle
            .channel_open_session()
            .await
            .map_err(|error| ConnectError::PtyRejected {
                reason: error.to_string(),
            })?;
    let size = request.size;
    channel
        .request_pty(
            true,
            TERM_VALUE,
            size.cols,
            size.rows,
            0,
            0,
            &[] as &[(russh::Pty, u32)],
        )
        .await
        .map_err(|error| ConnectError::PtyRejected {
            reason: error.to_string(),
        })?;
    channel
        .request_shell(true)
        .await
        .map_err(|error| ConnectError::PtyRejected {
            reason: error.to_string(),
        })?;

    let _ = events.send(RemoteEvent::Connected);

    let (mut read_half, write_half) = channel.split();
    let mut exit_code: Option<u32> = None;
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(Command::Close) => {
                    let _ = write_half.eof().await;
                    let _ = write_half.close().await;
                    let _ = handle
                        .disconnect(Disconnect::ByApplication, "", "")
                        .await;
                    return Ok(());
                }
                Some(Command::Write(bytes)) => {
                    if let Err(error) = write_half.data_bytes(bytes).await {
                        return Err(ConnectError::Disconnected {
                            reason: error.to_string(),
                        });
                    }
                }
                Some(Command::Resize(next)) => {
                    if let Err(error) = write_half
                        .window_change(next.cols, next.rows, 0, 0)
                        .await
                    {
                        return Err(ConnectError::Disconnected {
                            reason: error.to_string(),
                        });
                    }
                }
            },
            message = read_half.wait() => match message {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    // A terminal shows stderr and stdout in one grid, so both arrive
                    // as output; the distinction the protocol makes is not one the
                    // pane can draw.
                    if events.send(RemoteEvent::Data(data.to_vec())).is_err() {
                        return Ok(());
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    exit_code = Some(exit_status);
                }
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                    let _ = events.send(RemoteEvent::Exited { code: exit_code });
                    let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
                    return Ok(());
                }
                Some(_) => {}
            },
        }
    }
}

/// The credentials of one attempt, resolved before the handshake starts.
enum Material {
    Password(shellune_core::SecretString),
    Key(Arc<russh::keys::PrivateKey>),
    Agent {
        kind: AgentKind,
        client: agent::DynamicAgent,
    },
}

impl Material {
    async fn resolve(auth: AuthMethod) -> Result<Self, ConnectError> {
        match auth {
            AuthMethod::Password { password } => Ok(Self::Password(password)),
            AuthMethod::KeyFile { path, passphrase } => {
                let key = load_private_key(&path, passphrase.as_ref())?;
                Ok(Self::Key(Arc::new(key)))
            }
            AuthMethod::Agent { kind } => {
                let client = agent::connect(kind).await?;
                Ok(Self::Agent { kind, client })
            }
        }
    }
}

/// Tries the one authentication method the attempt chose.
async fn authenticate(
    handle: &mut client::Handle<ClientHandler>,
    user: &str,
    material: Material,
) -> Result<(), ConnectError> {
    match material {
        Material::Password(password) => {
            let accepted = handle
                .authenticate_password(user, password.expose())
                .await
                .map_err(ConnectError::from)?;
            if accepted.success() {
                Ok(())
            } else {
                Err(ConnectError::AuthRejected)
            }
        }
        Material::Key(key) => {
            let hash_alg = rsa_hash_alg(handle, &key).await;
            let accepted = handle
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(key, hash_alg))
                .await
                .map_err(ConnectError::from)?;
            if accepted.success() {
                Ok(())
            } else {
                Err(ConnectError::AuthRejected)
            }
        }
        Material::Agent { kind, mut client } => {
            let identities = client.request_identities().await.map_err(|error| {
                ConnectError::AgentUnavailable {
                    agent: kind,
                    reason: error.to_string(),
                }
            })?;
            if identities.is_empty() {
                return Err(ConnectError::AgentHasNoIdentity { agent: kind });
            }
            for identity in identities {
                let public = identity.public_key().into_owned();
                match handle
                    .authenticate_publickey_with(user, public, None, &mut client)
                    .await
                {
                    Ok(result) if result.success() => return Ok(()),
                    Ok(_) => {}
                    Err(error) => {
                        return Err(ConnectError::AgentSignFailed {
                            agent: kind,
                            reason: error.to_string(),
                        });
                    }
                }
            }
            Err(ConnectError::AgentIdentityRejected { agent: kind })
        }
    }
}

/// The RSA hash algorithm the server supports, which is None for other key types.
async fn rsa_hash_alg(
    handle: &client::Handle<ClientHandler>,
    key: &russh::keys::PrivateKey,
) -> Option<HashAlg> {
    if !matches!(key.algorithm(), russh::keys::Algorithm::Rsa { .. }) {
        return None;
    }
    match handle.best_supported_rsa_hash().await {
        Ok(Some(Some(hash_alg))) => Some(hash_alg),
        _ => None,
    }
}

/// The client handler: the one place that decides whether a server identity is used.
struct ClientHandler {
    host: String,
    port: u16,
    known: KnownHostKey,
    events: UnboundedSender<RemoteEvent>,
    decisions: UnboundedReceiver<TrustAnswer>,
    recorded: Arc<Mutex<Option<ConnectError>>>,
}

impl ClientHandler {
    fn record(&self, error: ConnectError) {
        if let Ok(mut slot) = self.recorded.lock() {
            *slot = Some(error);
        }
    }
}

impl client::Handler for ClientHandler {
    type Error = ConnectError;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let Some(presented) = PresentedKey::from_server_key(server_public_key) else {
            self.record(ConnectError::HostKeyUnsupported {
                reason: "the presented key has no SHA256 fingerprint".to_owned(),
            });
            return Ok(false);
        };

        match &self.known {
            KnownHostKey::Trusted { fingerprint } if *fingerprint == presented.fingerprint => {
                Ok(true)
            }
            KnownHostKey::Trusted { fingerprint } => {
                // A trusted host that answers with another key is a change, even
                // though the trust store called it trusted a moment ago.
                self.record(ConnectError::HostKeyChanged {
                    stored: fingerprint.clone(),
                    presented,
                });
                Ok(false)
            }
            KnownHostKey::Changed { fingerprint, .. } => {
                self.record(ConnectError::HostKeyChanged {
                    stored: fingerprint.clone(),
                    presented,
                });
                Ok(false)
            }
            KnownHostKey::Unknown => {
                let _ = self.events.send(RemoteEvent::HostKeyPrompt(HostKeyPrompt {
                    host: self.host.clone(),
                    port: self.port,
                    presented: presented.clone(),
                }));
                match self.decisions.recv().await {
                    Some(TrustAnswer::Accept) => Ok(true),
                    _ => {
                        self.record(ConnectError::HostKeyRejected { presented });
                        Ok(false)
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_is_kept_inside_what_a_terminal_server_accepts() {
        assert_eq!(PtySize::new(0, 0), PtySize { cols: 1, rows: 1 });
        assert_eq!(
            PtySize::new(4000, 4000),
            PtySize {
                cols: PtySize::MAX,
                rows: PtySize::MAX
            }
        );
        assert_eq!(PtySize::default(), PtySize { cols: 80, rows: 24 });
    }

    #[test]
    fn a_request_names_the_address_the_errors_will_use() {
        let request = ConnectRequest::new(
            "web.example.org",
            2222,
            "deploy",
            AuthMethod::password("hunter2"),
            PtySize::default(),
            KnownHostKey::Unknown,
        );
        assert_eq!(request.target(), "deploy@web.example.org:2222");
    }

    #[test]
    fn an_unreachable_address_fails_without_a_session() {
        // A port nothing listens on: the attempt must report a real transport
        // failure, and it must never report Connected first.
        let request = ConnectRequest::new(
            "127.0.0.1",
            1,
            "nobody",
            AuthMethod::password("hunter2"),
            PtySize::default(),
            KnownHostKey::Trusted {
                fingerprint: "SHA256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            },
        );
        let mut session = RemoteSession::connect(request);
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let events = session.drain();
            for event in &events {
                match event {
                    RemoteEvent::Failed(error) => {
                        assert!(
                            error.is_transport_failure(),
                            "port 1 refuses the connection: {error}"
                        );
                        assert!(!error.to_string().contains("hunter2"));
                        return;
                    }
                    RemoteEvent::Connected => {
                        panic!("an unreachable host must never report a live session")
                    }
                    _ => {}
                }
            }
            if std::time::Instant::now() > deadline {
                panic!("the attempt neither failed nor connected");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_write_after_the_attempt_ended_reports_the_closed_channel() {
        let request = ConnectRequest::new(
            "127.0.0.1",
            1,
            "nobody",
            AuthMethod::password("hunter2"),
            PtySize::default(),
            KnownHostKey::Unknown,
        );
        let mut session = RemoteSession::connect(request);
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            if session
                .drain()
                .iter()
                .any(|event| matches!(event, RemoteEvent::Failed(_)))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // The thread is gone, so the command channel is closed and writing says so
        // instead of silently dropping the bytes.
        let mut closed = false;
        for _ in 0..100 {
            if session.write(b"x").is_err() {
                closed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(closed, "a write to an ended attempt must report itself");
    }
}
