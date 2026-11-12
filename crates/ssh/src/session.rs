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
use shellune_core::{
    AgentKind, AuthMethod, ConnectionPolicy, DEFAULT_CONNECT_TIMEOUT_SECS,
    DEFAULT_KEEPALIVE_INTERVAL_SECS, X11Forwarding,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

use crate::agent;
use crate::auth::load_private_key;
use crate::error::ConnectError;
use crate::forwarding::{self, ForwardLink};
use crate::hostkey::{HostKeyPrompt, KnownHostKey, PresentedKey};
use crate::sftp::{self, ChannelFailure, FileLink};
use crate::transfer::{self, TransferLink};
use crate::x11::{self, X11Gate, X11Notice, X11Status};

/// Value of the TERM variable the remote shell is told about.
///
/// The same value the local PTY uses, so a program that looks right locally looks
/// the same over SSH.
pub const TERM_VALUE: &str = "xterm-256color";

/// The deadline the default connection policy uses for the machine part of an attempt.
///
/// It is a deadline for work no one has to think about: dialling, the key exchange
/// and the moment the server identity is known. Once the user is asked about an
/// unknown identity the clock is stopped, because the decision is theirs and no
/// deadline of ours may overrule it; a session that is still waiting for that answer
/// is reported as undecided, never as a timeout.
///
/// A host or the global preferences may choose another value (R20); this constant is
/// the value that applies when neither did, and it is the same number
/// [`shellune_core::ConnectionDefaults::default`] carries.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS as u64);

/// The keepalive interval the default connection policy probes at (R20).
///
/// No connection is dialled with this constant any more: a session uses the policy it
/// was handed, which is this value for a host that says nothing. It stays public
/// because it is the documented default a reader compares a saved choice against.
pub const KEEPALIVE_INTERVAL: Duration =
    Duration::from_secs(DEFAULT_KEEPALIVE_INTERVAL_SECS as u64);

/// The SSH library configuration one resolved policy produces (R20).
///
/// A direct session and every hop of a jump chain build their configuration here, so
/// the two paths cannot drift: an interval the user chose is the interval each hop
/// probes at, and a switched-off keepalive is `None` rather than a library default
/// nobody can see.
///
/// The inactivity timeout is written from the policy on purpose, and the policy always
/// answers `None`: that field closes a session which is merely quiet, while a healthy
/// idle shell is exactly that. Keepalive is the probe that tells a quiet link from a
/// dead one.
#[must_use]
pub fn client_config(policy: &ConnectionPolicy) -> client::Config {
    client::Config {
        nodelay: true,
        keepalive_interval: policy.keepalive_interval(),
        keepalive_max: policy.keepalive_max(),
        inactivity_timeout: policy.inactivity_timeout(),
        ..Default::default()
    }
}

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
    /// What this attempt is dialled with: keepalive and the connect deadline (R20).
    ///
    /// [ConnectionPolicy::default] unless the caller resolved a host's choice against
    /// the global defaults. The policy is what reaches the SSH library, so a choice the
    /// window shows is the choice this session actually uses.
    pub policy: ConnectionPolicy,
    /// Whether this connection opens a shell. A pure forward leaves it false.
    open_shell: bool,
    /// How this attempt forwards X11 to the local display (R11).
    ///
    /// Off unless the caller says otherwise: an attempt that never asked for a
    /// display channel is what every session did before X11 forwarding existed.
    x11: X11Forwarding,
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
            policy: ConnectionPolicy::default(),
            open_shell: true,
            x11: X11Forwarding::Off,
        }
    }

    /// The same connection without a shell channel.
    ///
    /// Authentication and host trust stay the same. The session reports
    /// [`RemoteEvent::Connected`] when those succeed and then only carries
    /// forwarding, SFTP and transfers.
    #[must_use]
    pub fn without_shell(mut self) -> Self {
        self.open_shell = false;
        self
    }

    /// The same request, asking the server for X11 forwarding.
    ///
    /// The mode belongs to this attempt alone and is not stored on a host record.
    #[must_use]
    pub fn with_x11(mut self, mode: X11Forwarding) -> Self {
        self.x11 = mode;
        self
    }

    /// The same request with another deadline for the machine part.
    ///
    /// Only the deadline changes; the keepalive stays what the policy already said.
    /// A test uses this so it does not wait twenty real seconds, and the window uses
    /// the policy builder below when a host or the preferences chose a value.
    #[must_use]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.policy = self.policy.with_connect_timeout(timeout);
        self
    }

    /// The same request with a resolved connection policy (R20).
    ///
    /// The policy carries the keepalive and the deadline together, because they are
    /// one answer to one question: this is how the attempt should treat a quiet link.
    #[must_use]
    pub fn with_connection_policy(mut self, policy: ConnectionPolicy) -> Self {
        self.policy = policy;
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
    /// What this session's X11 forwarding is doing (R11).
    ///
    /// A failure here is a status, not a session failure: the shell keeps working
    /// without a forwarded display.
    X11(X11Status),
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
    /// Open a session channel that runs one remote command.
    ///
    /// The traditional SCP copy needs this: the server runs its own `scp` and the
    /// channel carries that program's control records. The command text is built and
    /// quoted where the paths are known, and nothing else is ever run through it.
    Exec {
        /// The one command line, already quoted for the remote shell.
        command: String,
        /// Where the opened channel is sent.
        reply: oneshot::Sender<Result<Channel<client::Msg>, ChannelFailure>>,
    },
    /// Start or stop one forwarding rule on this connection.
    Forward(forwarding::ForwardCommand),
    /// Open one `direct-tcpip` channel for a listener that is already up.
    ///
    /// The accept loop cannot open the channel itself: this task owns the
    /// connection handle. The reply is how that loop learns the result.
    DirectTcp {
        /// Target host, as the client asked for it.
        host: String,
        /// Target port.
        port: u32,
        /// Address the local socket saw the client on.
        origin_host: String,
        /// Port the local socket saw the client on.
        origin_port: u32,
        /// Where the opened channel is sent.
        reply: oneshot::Sender<Result<Channel<client::Msg>, russh::Error>>,
    },
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
    /// The forwarding side of this connection, until the window takes it.
    forwards: Option<ForwardLink>,
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
        let (forward_link, forward_events, forward_gate) = forwarding::channel(commands.clone());
        // The X11 side exists before the connection does: the handler has to know it
        // before a server can open the first display channel.
        let (x11_gate, x11_events) = X11Gate::channel(request.x11);
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
                        forwards: forward_events,
                        gate: forward_gate,
                        x11: x11_gate,
                        x11_events,
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
            forwards: Some(forward_link),
        }
    }

    /// Takes the forwarding side of this connection, once.
    ///
    /// The rules then run on the connection that is already authenticated.
    /// None means the link was already taken, or the session has no server.
    pub fn take_forward_link(&mut self) -> Option<ForwardLink> {
        self.forwards.take()
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

    /// Opens one session channel that runs one command, for the batch runner (R19).
    ///
    /// The channel is opened by the connection's own loop, which is the only owner of
    /// the connection handle; the caller then drives the channel it was given. It is
    /// the same path the SCP copy takes, so a batch step cannot invent a second way to
    /// run a command, and a batch run inherits this session's trust decision,
    /// authentication and connection policy.
    pub(crate) fn open_exec(
        &mut self,
        command: String,
    ) -> Result<oneshot::Receiver<Result<Channel<client::Msg>, ChannelFailure>>, String> {
        if self.closed {
            return Err("the session is already closed".to_owned());
        }
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Exec { command, reply })
            .map_err(|_| "the connection is gone".to_owned())?;
        Ok(answer)
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
    forwards: tokio::sync::mpsc::UnboundedSender<forwarding::ForwardEvent>,
    gate: std::sync::Arc<forwarding::ForwardGate>,
    /// This session's X11 forwarding, shared with the handler.
    x11: Arc<X11Gate>,
    x11_events: tokio::sync::mpsc::UnboundedReceiver<X11Notice>,
    open: UnboundedSender<Command>,
}

/// Runs one attempt to completion, reporting everything through the event channel.
async fn run(request: ConnectRequest, channels: Channels) {
    let events = channels.events.clone();
    let gate = Arc::clone(&channels.x11);
    let result = connect_and_shell(request, channels).await;
    // Whatever ended the session, no bridge may stay open: closing stops accepting
    // new display channels before the open ones are dropped.
    gate.close();
    if let Err(error) = result {
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
        forwards: forward_events,
        gate,
        x11: x11_gate,
        mut x11_events,
        open: open_commands,
    } = channels;
    let target = request.target();
    let open_shell = request.open_shell;
    let x11_mode = request.x11;
    let material = Material::resolve(request.auth).await?;

    let config = Arc::new(client_config(&request.policy));
    let recorded = Arc::new(Mutex::new(None::<ConnectError>));
    let progress = Arc::new(HandshakeProgress::default());
    let mut carrier_lost = gate.subscribe_lost();
    let handler = ClientHandler {
        host: request.host.clone(),
        port: request.port,
        known: request.known.clone(),
        events: events.clone(),
        decisions,
        cancelled,
        progress: Arc::clone(&progress),
        recorded: Arc::clone(&recorded),
        gate: Arc::clone(&gate),
        x11: Arc::clone(&x11_gate),
    };

    let mut handle = match dial(
        config,
        (request.host.as_str(), request.port),
        handler,
        &progress,
        request.policy.connect_timeout(),
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

    let shell = if open_shell {
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
        // A display server that is not there is reported before the request is sent,
        // and never as a reason to stop the connection: the shell below is opened
        // exactly as it would have been.
        if !x11_mode.is_enabled() {
            // A session that forwards nothing says so, instead of leaving the reader to
            // guess whether a display channel was requested.
            let _ = events.send(RemoteEvent::X11(X11Status::Off));
        }
        let prepared = if x11_mode.is_enabled() {
            // A display server that is not there, an authority file without a cookie and
            // a display that cannot issue a restricted cookie are all reported before
            // the request is sent, and never as a reason to stop the connection: the
            // shell below is opened exactly as it would have been.
            match x11::arm(x11_mode).await {
                Ok(prepared) => Some(prepared),
                Err(failure) => {
                    let _ = events.send(RemoteEvent::X11(X11Status::Failed {
                        reason: failure.to_string(),
                        next_step: failure.next_step(),
                    }));
                    None
                }
            }
        } else {
            None
        };
        let requested = if let Some(prepared) = prepared {
            let requested = channel
                .request_x11(
                    true,
                    false,
                    x11::AUTH_NAME,
                    prepared.request_cookie(),
                    prepared.screen(),
                )
                .await;
            match requested {
                Ok(()) => {
                    x11_gate.announce(&prepared);
                    true
                }
                Err(error) => {
                    let failure = x11::X11Failure::Lost {
                        detail: error.to_string(),
                    };
                    let _ = events.send(RemoteEvent::X11(X11Status::Failed {
                        reason: failure.to_string(),
                        next_step: failure.next_step(),
                    }));
                    false
                }
            }
        } else {
            false
        };
        // The answers to the pseudo-terminal, the X11 request and the shell request
        // arrive in that order, which is how the X11 one is recognised.
        x11_gate.shell_requests(channel.id().number(), requested);
        channel
            .request_shell(true)
            .await
            .map_err(|error| ConnectError::PtyRejected {
                reason: error.to_string(),
            })?;
        Some(channel.split())
    } else {
        None
    };

    let _ = events.send(RemoteEvent::Connected);

    // The file browser works on this same authenticated connection: its task asks
    // the loop below for an SFTP channel and then runs on its own, so reading a
    // directory never stops the terminal from reading or writing. A pure forward
    // keeps the same path and simply never opens a shell channel.
    tokio::spawn(sftp::serve(files, open_commands.clone()));
    tokio::spawn(transfer::serve(transfers, open_commands.clone()));
    let mut forwards = forwarding::ForwardLoop::new(forward_events, gate, open_commands);

    if let Some((mut read_half, write_half)) = shell {
        let mut exit_code: Option<u32> = None;
        let mut x11_open = true;
        loop {
            tokio::select! {
                biased;
                changed = carrier_lost.changed() => {
                    if changed.is_err() || *carrier_lost.borrow() {
                        forwards.shutdown(&handle).await;
                        return Err(ConnectError::Disconnected {
                            reason: "the SSH connection closed".to_owned(),
                        });
                    }
                }
                notice = x11_events.recv(), if x11_open => {
                    match notice {
                        Some(X11Notice::Status(status)) => {
                            if events.send(RemoteEvent::X11(status)).is_err() {
                                forwards.shutdown(&handle).await;
                                return Ok(());
                            }
                        }
                        // A refusal is answered with what the server says about its own
                        // xauth, which is the only way the two server-side causes can be
                        // told apart. The answer is best effort and never blocks the
                        // session for long.
                        Some(X11Notice::Refused) => {
                            let status = x11::refusal_status(&handle).await;
                            if events.send(RemoteEvent::X11(status)).is_err() {
                                forwards.shutdown(&handle).await;
                                return Ok(());
                            }
                        }
                        None => x11_open = false,
                    }
                }
                command = commands.recv() => {
                    let Some(command) = command else {
                        let _ = write_half.eof().await;
                        let _ = write_half.close().await;
                        forwards.shutdown(&handle).await;
                        let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
                        return Ok(());
                    };
                    let Some(command) = forwards.accept_command(&handle, command).await else {
                        continue;
                    };
                    match command {
                        Command::Close => {
                            let _ = write_half.eof().await;
                            let _ = write_half.close().await;
                            forwards.shutdown(&handle).await;
                            let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
                            return Ok(());
                        }
                        Command::Write(bytes) => {
                            if let Err(error) = write_half.data_bytes(bytes).await {
                                forwards.shutdown(&handle).await;
                                return Err(ConnectError::Disconnected {
                                    reason: error.to_string(),
                                });
                            }
                        }
                        Command::Sftp(reply) => {
                            // One round trip, inside the loop: the channel has to be opened
                            // where the connection handle is, and this is the only owner.
                            let _ = reply.send(open_sftp_channel(&handle).await);
                        }
                        Command::Exec { command, reply } => {
                            let _ = reply.send(open_exec_channel(&handle, &command).await);
                        }
                        Command::Resize(next) => {
                            if let Err(error) = write_half
                                .window_change(next.cols, next.rows, 0, 0)
                                .await
                            {
                                forwards.shutdown(&handle).await;
                                return Err(ConnectError::Disconnected {
                                    reason: error.to_string(),
                                });
                            }
                        }
                        Command::Forward(_) | Command::DirectTcp { .. } => {}
                    }
                }
                message = read_half.wait() => match message {
                    Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                        // A terminal shows stderr and stdout in one grid, so both arrive
                        // as output; the distinction the protocol makes is not one the
                        // pane can draw.
                        if events.send(RemoteEvent::Data(data.to_vec())).is_err() {
                            forwards.shutdown(&handle).await;
                            return Ok(());
                        }
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => {
                        exit_code = Some(exit_status);
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                        let _ = events.send(RemoteEvent::Exited { code: exit_code });
                        forwards.shutdown(&handle).await;
                        let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
                        return Ok(());
                    }
                    Some(_) => {}
                },
            }
        }
    }

    loop {
        tokio::select! {
            biased;
            changed = carrier_lost.changed() => {
                if changed.is_err() || *carrier_lost.borrow() {
                    forwards.shutdown(&handle).await;
                    return Err(ConnectError::Disconnected {
                        reason: "the SSH connection closed".to_owned(),
                    });
                }
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    forwards.shutdown(&handle).await;
                    let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
                    return Ok(());
                };
                let Some(command) = forwards.accept_command(&handle, command).await else {
                    continue;
                };
                match command {
                    Command::Close => {
                        forwards.shutdown(&handle).await;
                        let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
                        return Ok(());
                    }
                    Command::Sftp(reply) => {
                        let _ = reply.send(open_sftp_channel(&handle).await);
                    }
                    Command::Exec { command, reply } => {
                        let _ = reply.send(open_exec_channel(&handle, &command).await);
                    }
                    // A pure forward has no shell to write to or resize.
                    Command::Write(_) | Command::Resize(_) | Command::Forward(_) | Command::DirectTcp { .. } => {}
                }
            }
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

/// Opens a session channel and asks the server to run one command on it.
///
/// This is what the traditional SCP copy needs: the server runs its own copy program,
/// and the channel is the pipe that program speaks its control records on. The text is
/// the caller's, already quoted; a server that refuses the request answers on the
/// channel itself, which is why an accepted request is not the same as a started
/// program.
pub(crate) async fn open_exec_channel<H: client::Handler>(
    handle: &client::Handle<H>,
    command: &str,
) -> Result<Channel<client::Msg>, ChannelFailure> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|error| ChannelFailure::Lost {
            reason: error.to_string(),
        })?;
    channel
        .exec(true, command)
        .await
        .map_err(|error| ChannelFailure::Lost {
            reason: error.to_string(),
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
    gate: Arc<forwarding::ForwardGate>,
    /// This session's X11 forwarding: inbound display channels are bridged here.
    x11: Arc<X11Gate>,
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

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<client::Msg>,
        connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: russh::client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        forwarding::open_inbound(
            &self.gate,
            channel,
            connected_address,
            connected_port,
            reply,
        )
        .await;
        Ok(())
    }

    /// A server opened an x11 channel for a program of this session.
    ///
    /// It is bridged to this machine's display, with this machine's cookie; a session
    /// that never asked for forwarding refuses the channel.
    async fn server_channel_open_x11(
        &mut self,
        channel: Channel<client::Msg>,
        _originator_address: &str,
        _originator_port: u32,
        reply: russh::client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        self.x11.open_inbound(channel, reply).await;
        Ok(())
    }

    /// The server accepted a channel request; the answers arrive in request order.
    async fn channel_success(
        &mut self,
        channel: russh::ChannelId,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        self.x11.on_channel_reply(channel.number(), true);
        Ok(())
    }

    /// The server refused a channel request, which for x11-req is a refusal to
    /// forward at all.
    async fn channel_failure(
        &mut self,
        channel: russh::ChannelId,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        self.x11.on_channel_reply(channel.number(), false);
        Ok(())
    }

    async fn disconnected(
        &mut self,
        reason: client::DisconnectReason<Self::Error>,
    ) -> Result<(), Self::Error> {
        self.gate.mark_lost();
        match reason {
            client::DisconnectReason::ReceivedDisconnect(_) => Ok(()),
            client::DisconnectReason::Error(error) => Err(error),
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
                forwards: None,
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
    fn the_policy_is_what_reaches_the_ssh_library() {
        let defaults = shellune_core::ConnectionDefaults::default();
        let config = client_config(&defaults.policy());
        assert_eq!(
            config.keepalive_interval,
            Some(Duration::from_secs(30)),
            "a host that says nothing probes every 30 seconds"
        );
        assert_eq!(config.keepalive_max, 3);
        assert_eq!(
            config.inactivity_timeout, None,
            "an idle shell that answers is never closed for silence"
        );

        let own = shellune_core::HostConnection::new(Some(90), Some(6), Some(9)).expect("valid");
        let config = client_config(&defaults.policy_for(Some(own)));
        assert_eq!(config.keepalive_interval, Some(Duration::from_secs(90)));
        assert_eq!(config.keepalive_max, 6);
        assert_eq!(config.inactivity_timeout, None);

        let off = defaults.policy_for(Some(shellune_core::HostConnection::keepalive_off()));
        let config = client_config(&off);
        assert_eq!(
            config.keepalive_interval, None,
            "an explicit off sends no probe at all"
        );
        assert_eq!(config.inactivity_timeout, None);

        let disabled = shellune_core::ConnectionDefaults::new(0, 3, 20).expect("valid");
        assert_eq!(client_config(&disabled.policy()).keepalive_interval, None);
        assert_eq!(
            client_config(&disabled.policy()).inactivity_timeout,
            None,
            "switching keepalive off does not switch an inactivity timer on"
        );
    }

    #[test]
    fn a_request_carries_the_policy_and_the_deadline_it_implies() {
        let request = ConnectRequest::new(
            "web.example.org",
            2222,
            "deploy",
            AuthMethod::password("hunter2"),
            PtySize::default(),
            KnownHostKey::Unknown,
        );
        assert_eq!(request.policy, ConnectionPolicy::default());
        assert_eq!(request.policy.connect_timeout(), CONNECT_TIMEOUT);

        let chosen = shellune_core::ConnectionDefaults::new(45, 5, 7)
            .expect("valid")
            .policy();
        let request = request.with_connection_policy(chosen);
        assert_eq!(
            request.policy.keepalive_interval(),
            Some(Duration::from_secs(45))
        );
        assert_eq!(request.policy.connect_timeout(), Duration::from_secs(7));

        let shortened = request.with_connect_timeout(Duration::from_secs(3));
        assert_eq!(
            shortened.policy.connect_timeout(),
            Duration::from_secs(3),
            "the deadline the dial uses is the policy's"
        );
        assert_eq!(
            shortened.policy.keepalive_interval(),
            Some(Duration::from_secs(45)),
            "changing the deadline leaves the keepalive choice alone"
        );
    }

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
