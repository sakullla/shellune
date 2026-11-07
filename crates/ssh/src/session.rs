//! One remote session: host trust, one authentication method and a real remote PTY.
//!
//! The shape of this module follows the local PTY in shellune-terminal on purpose:
//! a background thread owns the async runtime, the shell polls for events and sends
//! commands, and nothing in the window ever awaits the network. That is what lets
//! the same terminal pane drive a remote shell without knowing that SSH exists.
//!
//! Four rules hold:
//!
//! * an unknown identity is only used after the user answers the question, so
//!   check_server_key blocks on a decision instead of guessing;
//! * the connect deadline belongs to the machine, not to the user: it covers TCP and
//!   the key exchange up to the question about the identity, and stops there, so
//!   comparing a fingerprint for a minute is never reported as a timeout;
//! * a changed identity ends the attempt and is never turned into an "accept the new
//!   key" flow, because the trust record must not be overwritten by a connection
//!   that was not deliberately re-authorised;
//! * exactly one authentication method is tried. A failure is reported as that
//!   method failing, and the user chooses another one, which is why nothing here
//!   loops over methods behind the user's back.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg};
use russh::{Channel, ChannelMsg, Disconnect};
use shellune_core::{AgentKind, AuthMethod};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

use crate::agent;
use crate::auth::load_private_key;
use crate::error::ConnectError;
use crate::hostkey::{HostKeyPrompt, KnownHostKey, PresentedKey};
use crate::sftp::{self, ChannelFailure, FileLink};
use crate::transfer::{self, TransferLink};

/// Value of the TERM variable the remote shell is told about.
///
/// The same value the local PTY uses, so a program that looks right locally looks
/// the same over SSH.
pub const TERM_VALUE: &str = "xterm-256color";

/// How long the machine part of an attempt may take.
///
/// It is a deadline for work no one has to think about: dialling, the key exchange
/// and the moment the server identity is known. Once the user is asked about an
/// unknown identity the clock is stopped, because the decision is theirs and no
/// deadline of ours may overrule it; a session that is still waiting for that answer
/// is reported as undecided, never as a timeout.
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

/// How far a handshake got, as far as the connect deadline is concerned.
///
/// The attempt moves through these three stages once, in this order, and the
/// deadline only applies during the first: everything after it is the user's work or
/// the consequence of the user's decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandshakeStage {
    /// TCP and the key exchange, before anything is asked of the user.
    Machine = 0,
    /// The identity question is on its way to the user; no deadline applies.
    WaitingForUser = 1,
    /// The user decided, or no decision was needed. Nothing here is timed out.
    UserDecided = 2,
}

/// The stage of the handshake, shared between the deadline and the handler.
///
/// The handler runs inside the connect future the deadline wraps, so it can only
/// report progress through a value like this one.
#[derive(Debug, Default)]
pub(crate) struct HandshakeProgress {
    stage: AtomicU8,
}

impl HandshakeProgress {
    pub(crate) fn stage(&self) -> HandshakeStage {
        match self.stage.load(Ordering::SeqCst) {
            1 => HandshakeStage::WaitingForUser,
            2 => HandshakeStage::UserDecided,
            _ => HandshakeStage::Machine,
        }
    }

    pub(crate) fn set(&self, stage: HandshakeStage) {
        self.stage.store(stage as u8, Ordering::SeqCst);
    }
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
    /// How long the machine part of the handshake may take.
    ///
    /// [CONNECT_TIMEOUT] unless a test needs the same behaviour without waiting
    /// twenty real seconds for it.
    pub connect_timeout: Duration,
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
            connect_timeout: CONNECT_TIMEOUT,
        }
    }

    /// The same request with another deadline for the machine part.
    #[must_use]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
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
pub(crate) enum Command {
    Write(Vec<u8>),
    Resize(PtySize),
    Close,
    /// Open an SFTP subsystem channel for the file browser of this connection.
    ///
    /// The channel is opened here because this loop is the only owner of the
    /// connection handle; the answer is dropped unanswered when the loop ends, which
    /// is how a browser waiting for it learns the connection is gone.
    Sftp(oneshot::Sender<Result<Channel<client::Msg>, ChannelFailure>>),
}

/// A remote session, from the first handshake to its last byte.
pub struct RemoteSession {
    commands: UnboundedSender<Command>,
    decisions: UnboundedSender<TrustAnswer>,
    /// Tells the attempt to stop waiting for an answer it will never get.
    cancelled: UnboundedSender<()>,
    events: UnboundedReceiver<RemoteEvent>,
    size: PtySize,
    closed: bool,
    /// The browsable side of this connection, until a file view takes it.
    files: Option<FileLink>,
    /// The transfer side of this connection, until the window takes it.
    transfers: Option<TransferLink>,
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
        let (cancelled, cancel_rx) = unbounded_channel();
        // The file browser of this connection: requests go to the connection thread,
        // answers come back here. Both exist before the connection does, so a file
        // view opened at any point has somewhere to ask and somewhere to listen.
        let (file_requests, file_request_rx) = unbounded_channel();
        let (file_responses, file_response_rx) = unbounded_channel();
        let files = sftp::Wiring {
            requests: file_request_rx,
            responses: file_responses,
        };
        // The transfer tasks of this connection: the window sends commands, the tasks
        // report progress and outcomes with the identifier of the task they belong to.
        let (transfer_commands, transfer_command_rx) = unbounded_channel();
        let (transfer_events, transfer_event_rx) = unbounded_channel();
        let transfers = transfer::Wiring {
            commands: transfer_command_rx,
            events: transfer_events,
        };
        let size = request.size;
        // The file browser asks the loop for its channel through the same command
        // channel the terminal uses, so it needs a sender of its own.
        let open_commands = commands.clone();

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
                runtime.block_on(run(
                    request,
                    Channels {
                        commands: command_rx,
                        events,
                        decisions: decision_rx,
                        cancelled: cancel_rx,
                        files,
                        transfers,
                        open: open_commands,
                    },
                ));
            })
            .expect("the SSH thread can be created");

        Self {
            commands,
            decisions,
            cancelled,
            events: event_rx,
            size,
            closed: false,
            files: Some(FileLink::new(file_requests, file_response_rx)),
            transfers: Some(TransferLink::new(transfer_commands, transfer_event_rx)),
        }
    }

    /// Takes the transfer side of this connection.
    ///
    /// Taken once, by the window, for the same reason [`RemoteSession::take_file_link`]
    /// is: the transfers then run on the connection the shell already authenticated and
    /// never ask for credentials a second time. None means it was already taken.
    pub fn take_transfer_link(&mut self) -> Option<TransferLink> {
        self.transfers.take()
    }

    /// Takes the browsable side of this connection.
    ///
    /// It is taken once, by the window, before the terminal owns the session: the
    /// file view then keeps asking on this connection, which is why browsing a
    /// server never asks for credentials a second time. None means the link was
    /// already taken, or that the session was created without one.
    pub fn take_file_link(&mut self) -> Option<FileLink> {
        self.files.take()
    }

    /// Answers the question about an unknown identity.
    ///
    /// Returns whether the answer could be delivered. A caller that stores a trust
    /// record because of this answer must check the result: false means the question
    /// is no longer being asked — the attempt already ended, or it was closed — so
    /// the record would claim a decision no connection ever used.
    pub fn answer_host_key(&mut self, accept: bool) -> bool {
        if self.closed {
            return false;
        }
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
        // A form that closes while its identity question is on screen must also end
        // the question: otherwise the attempt keeps waiting for an answer nobody can
        // give any more, and the failure it eventually reports is a timeout for a
        // server that answered at once.
        let _ = self.cancelled.send(());
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

/// Everything one attempt reports to and takes commands from.
///
/// They travel as one value because they are created together and all belong to the same
/// connection: the terminal's commands, the answers to the identity question, the two
/// subsystem wirings, and the channel a browser or a transfer opens its own channel
/// through.
struct Channels {
    commands: UnboundedReceiver<Command>,
    events: UnboundedSender<RemoteEvent>,
    decisions: UnboundedReceiver<TrustAnswer>,
    cancelled: UnboundedReceiver<()>,
    files: sftp::Wiring,
    transfers: transfer::Wiring,
    open: UnboundedSender<Command>,
}

/// Runs one attempt to completion, reporting everything through the event channel.
async fn run(request: ConnectRequest, channels: Channels) {
    let events = channels.events.clone();
    if let Err(error) = connect_and_shell(request, channels).await {
        let _ = events.send(RemoteEvent::Failed(error));
    }
}

async fn connect_and_shell(
    request: ConnectRequest,
    channels: Channels,
) -> Result<(), ConnectError> {
    let Channels {
        mut commands,
        events,
        decisions,
        cancelled,
        files,
        transfers,
        open: open_commands,
    } = channels;
    let target = request.target();
    let material = Material::resolve(request.auth).await?;

    let config = Arc::new(client::Config {
        nodelay: true,
        keepalive_interval: Some(KEEPALIVE_INTERVAL),
        ..Default::default()
    });
    let recorded = Arc::new(Mutex::new(None::<ConnectError>));
    let progress = Arc::new(HandshakeProgress::default());
    let handler = ClientHandler {
        host: request.host.clone(),
        port: request.port,
        known: request.known.clone(),
        events: events.clone(),
        decisions,
        cancelled,
        progress: Arc::clone(&progress),
        recorded: Arc::clone(&recorded),
    };

    let mut handle = match dial(
        config,
        (request.host.as_str(), request.port),
        handler,
        &progress,
        request.connect_timeout,
    )
    .await
    {
        Ok(handle) => handle,
        Err(DialFailure::Timeout) => return Err(ConnectError::TimedOut { target }),
        Err(DialFailure::Attempt(error)) => {
            // The handler knows more than the transport error does: it saw the
            // identity that was refused, that changed, or that was never decided on,
            // so its verdict wins over the transport's own wording.
            let recorded = recorded.lock().ok().and_then(|slot| slot.clone());
            return Err(recorded.unwrap_or(error));
        }
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

    // The file browser works on this same authenticated connection: its task asks
    // the loop below for an SFTP channel and then runs on its own, so reading a
    // directory never stops the terminal from reading or writing.
    tokio::spawn(sftp::serve(files, open_commands.clone()));
    tokio::spawn(transfer::serve(transfers, open_commands));

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
                Some(Command::Sftp(reply)) => {
                    // One round trip, inside the loop: the channel has to be opened
                    // where the connection handle is, and this is the only owner.
                    let _ = reply.send(open_sftp_channel(&handle).await);
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

/// Opens an SFTP subsystem channel on the live connection.
///
/// A server without an SFTP subsystem answers the subsystem request with a failure.
/// That is a statement about the server's configuration and not about the shell, so
/// it is reported as its own kind of failure: the terminal keeps running, and the
/// file view says the server has no SFTP instead of claiming the session ended.
pub(crate) async fn open_sftp_channel<H: client::Handler>(
    handle: &client::Handle<H>,
) -> Result<Channel<client::Msg>, ChannelFailure> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|error| ChannelFailure::Lost {
            reason: error.to_string(),
        })?;
    channel
        .request_subsystem(true, sftp::SFTP_SUBSYSTEM)
        .await
        .map_err(|error| match error {
            russh::Error::RequestDenied => ChannelFailure::Unavailable {
                reason: "the server refused the sftp subsystem".to_owned(),
            },
            other => ChannelFailure::Lost {
                reason: other.to_string(),
            },
        })?;
    Ok(channel)
}

/// Why a dial stopped before it produced a connected handle.
pub(crate) enum DialFailure {
    /// The machine part of the handshake did not finish before the deadline.
    Timeout,
    /// The attempt ended with an error, which the handler may describe better.
    Attempt(ConnectError),
}

/// Runs the handshake, but only the machine part of it under a deadline.
///
/// The deadline covers the dial and the key exchange, which is work nobody has to
/// think about. It stops the moment the handler says the user is being asked about an
/// unknown identity, and it never comes back: comparing a fingerprint and deciding
/// what to do about it is the user's work, and a session that is still waiting for
/// that answer is reported as undecided rather than as a slow server.
async fn dial<A: tokio::net::ToSocketAddrs>(
    config: Arc<client::Config>,
    addrs: A,
    handler: ClientHandler,
    progress: &HandshakeProgress,
    timeout: Duration,
) -> Result<client::Handle<ClientHandler>, DialFailure> {
    finish_dial(client::connect(config, addrs, handler), progress, timeout).await
}

/// Runs a handshake future with the same deadline rules as a direct TCP dial.
///
/// The chain uses this for a stream that is already open — a jump channel or a
/// confirmed ProxyCommand — so the deadline still stops when the user is asked
/// about an identity and still does not apply to that decision.
pub(crate) async fn finish_dial<H, F>(
    connecting: F,
    progress: &HandshakeProgress,
    timeout: Duration,
) -> Result<client::Handle<H>, DialFailure>
where
    H: client::Handler,
    F: std::future::Future<Output = Result<client::Handle<H>, ConnectError>>,
{
    let mut connecting = Box::pin(connecting);
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        match progress.stage() {
            // From here on the attempt is either waiting for the user or carrying out
            // what the user decided; both finish without a deadline.
            HandshakeStage::WaitingForUser | HandshakeStage::UserDecided => {
                return connecting.as_mut().await.map_err(DialFailure::Attempt);
            }
            HandshakeStage::Machine => {}
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::select! {
            result = connecting.as_mut() => return result.map_err(DialFailure::Attempt),
            () = tokio::time::sleep(remaining) => {
                // The question may have reached the user during the very poll that was
                // running when the deadline passed; if it did, the wait is theirs.
                if progress.stage() == HandshakeStage::Machine {
                    return Err(DialFailure::Timeout);
                }
            }
        }
    }
}

/// The credentials of one attempt, resolved before the handshake starts.
pub(crate) enum Material {
    Password(shellune_core::SecretString),
    Key(Arc<russh::keys::PrivateKey>),
    Agent {
        kind: AgentKind,
        client: agent::DynamicAgent,
    },
}

impl Material {
    pub(crate) async fn resolve(auth: AuthMethod) -> Result<Self, ConnectError> {
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
pub(crate) async fn authenticate<H: client::Handler>(
    handle: &mut client::Handle<H>,
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
async fn rsa_hash_alg<H: client::Handler>(
    handle: &client::Handle<H>,
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
    /// Fires when the caller stopped wanting an answer: the form was closed.
    cancelled: UnboundedReceiver<()>,
    progress: Arc<HandshakeProgress>,
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
        // The server identity is now known, so the part of the handshake that can be
        // blamed on the network is over: from here on nothing is timed out.
        self.progress.set(HandshakeStage::UserDecided);

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
                // The user is asked, and the connect deadline stops here: a
                // fingerprint that takes a minute to compare is a decision in
                // progress, not a slow server.
                self.progress.set(HandshakeStage::WaitingForUser);
                let _ = self.events.send(RemoteEvent::HostKeyPrompt(HostKeyPrompt {
                    host: self.host.clone(),
                    port: self.port,
                    presented: presented.clone(),
                }));
                let answer = {
                    // Split borrows: both channels are awaited in the same select.
                    let Self {
                        decisions,
                        cancelled,
                        ..
                    } = self;
                    tokio::select! {
                        answer = decisions.recv() => answer,
                        // The form closed, or the session was dropped: nobody can
                        // answer any more, so the attempt ends as undecided instead
                        // of waiting for an answer that will never come.
                        _ = cancelled.recv() => None,
                    }
                };
                self.progress.set(HandshakeStage::UserDecided);
                match answer {
                    Some(TrustAnswer::Accept) => Ok(true),
                    Some(TrustAnswer::Reject) => {
                        self.record(ConnectError::HostKeyRejected { presented });
                        Ok(false)
                    }
                    None => {
                        self.record(ConnectError::HostKeyUndecided { presented });
                        Ok(false)
                    }
                }
            }
        }
    }
}

/// A session whose events are supplied by a test instead of a server.
///
/// The window has two states that a live server cannot be made to produce on
/// demand: a batch where the connection notice and the first output arrive
/// together, and an attempt that is still waiting for the identity question while
/// the user answers it or closes the form. Only the events are scripted — the
/// session type, its channels and everything the window does with them are the real
/// ones, so a test here exercises the same path a connection uses.
#[cfg(feature = "test-support")]
pub struct ScriptedSession {
    session: Option<RemoteSession>,
    events: UnboundedSender<RemoteEvent>,
    answers: Arc<Mutex<Vec<TrustAnswer>>>,
}

#[cfg(feature = "test-support")]
impl ScriptedSession {
    /// A session that has reported nothing yet.
    #[must_use]
    pub fn new() -> Self {
        let (commands, command_rx) = unbounded_channel();
        let (events, event_rx) = unbounded_channel();
        let (decisions, mut decision_rx) = unbounded_channel();
        let (cancelled, cancel_rx) = unbounded_channel();
        let answers = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&answers);
        thread::Builder::new()
            .name("shellune-ssh-scripted".to_owned())
            .spawn(move || {
                // Holding the command side open lets a test type into a scripted
                // pane; what matters here is recording the answers the window gives,
                // so a test can prove a decision really reached the transport.
                let _commands = command_rx;
                let _cancelled = cancel_rx;
                while let Some(answer) = decision_rx.blocking_recv() {
                    if let Ok(mut answers) = recorded.lock() {
                        answers.push(answer);
                    }
                }
            })
            .expect("the scripted session thread can be created");
        Self {
            session: Some(RemoteSession {
                commands,
                decisions,
                cancelled,
                events: event_rx,
                size: PtySize::default(),
                closed: false,
                // A scripted session has no server, so it has no browsable side and no
                // transfer side: the window's tests supply those answers themselves.
                files: None,
                transfers: None,
            }),
            events,
            answers,
        }
    }

    /// Reports an event, exactly as the transport would report it.
    pub fn push(&self, event: RemoteEvent) {
        let _ = self.events.send(event);
    }

    /// The session to hand to a pane or a window. It can be taken once.
    pub fn take_session(&mut self) -> RemoteSession {
        self.session
            .take()
            .expect("a scripted session is taken exactly once")
    }

    /// The answers the window has given so far.
    #[must_use]
    pub fn answers(&self) -> Vec<TrustAnswer> {
        self.answers
            .lock()
            .map(|answers| answers.clone())
            .unwrap_or_default()
    }
}

#[cfg(feature = "test-support")]
impl Default for ScriptedSession {
    fn default() -> Self {
        Self::new()
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
