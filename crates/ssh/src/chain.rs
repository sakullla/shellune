//! An ordered SSH jump chain with one trust decision and one ProxyCommand per hop.
//!
//! The chain is built from the saved host graph before anything is dialled. A
//! cycle, a missing hop, a missing credential or a host that asks for both a
//! jump list and a ProxyCommand stops the attempt with a message and leaves no
//! socket and no process behind. Every other hop is opened in order. Its stream
//! is a direct TCP connection, a direct-tcpip channel through the previous hop,
//! or a ProxyCommand the user has just confirmed. Trust and authentication
//! belong to that hop's host id, address and port, not to the final target.
//!
//! Cancelling or failing tears the chain down from the far end: the hop that
//! was still opening, then each earlier hop, including its child process. A hop
//! that is no longer asking refuses a late trust answer, so a closed node cannot
//! be accepted after the attempt has moved on or stopped.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::thread;
use std::time::Duration;

use russh::client::{self, Handler};
use russh::{Channel, ChannelMsg, Disconnect};
use shellune_core::{
    AuthMethod, ConnectionPolicy, HostConfig, HostId, X11Forwarding, connection_jump_fault,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::{oneshot, watch};

use crate::error::{ChainFailure, ConnectError, HopLocation};
use crate::forwarding::{self, ForwardLink};
use crate::hostkey::{HostKeyPrompt, KnownHostKey, PresentedKey};
use crate::session::{
    Command, DialFailure, HandshakeProgress, HandshakeStage, Material, PtySize, TERM_VALUE,
    authenticate, client_config, finish_dial, open_exec_channel, open_sftp_channel,
};
use crate::sftp::{self, ChannelFailure, FileLink};
use crate::transfer::{self, TransferLink};
use crate::transport::{self, Cancel, IoStream, ProxyProcess};
use crate::x11::{self, X11Gate, X11Notice, X11Status};

/// Credentials and the stored trust decision for one hop.
///
/// The password or passphrase stays inside [`AuthMethod`] and is moved into the
/// attempt. It is not cloned into a second chain and it is not written into an
/// event.
pub struct HopAuth {
    /// The saved host these credentials belong to.
    pub host_id: HostId,
    /// The one authentication method this hop will try.
    pub auth: AuthMethod,
    /// What the trust records say about this hop before the attempt.
    pub known: KnownHostKey,
}

impl HopAuth {
    /// One hop's credentials and trust decision.
    #[must_use]
    pub fn new(host_id: HostId, auth: AuthMethod, known: KnownHostKey) -> Self {
        Self {
            host_id,
            auth,
            known,
        }
    }
}

/// What the chain reports, each of them tied to the hop that caused it.
#[derive(Debug)]
pub enum ChainEvent {
    /// This hop is about to be dialled.
    HopStarted(HopLocation),
    /// This hop's server identity is unknown and has to be decided.
    HostKeyPrompt {
        /// The hop that presented the key.
        hop: HopLocation,
        /// The question, addressed to that hop's host and port.
        prompt: HostKeyPrompt,
    },
    /// A ProxyCommand will not run until it is confirmed.
    ProxyConfirmation {
        /// The hop the command would reach.
        hop: HopLocation,
        /// The command with tokens expanded. This is what would be executed.
        command: String,
    },
    /// This hop accepted the credentials. It is not the target shell yet.
    HopReady(HopLocation),
    /// The target shell is running on this hop.
    Connected {
        /// The target hop.
        hop: HopLocation,
    },
    /// Bytes the target shell produced.
    Data(Vec<u8>),
    /// The target shell ended.
    Exited {
        /// Exit status, when the server reported one.
        code: Option<u32>,
    },
    /// What the target's X11 forwarding is doing (R11).
    ///
    /// A failure here is not a session failure: the terminal keeps working, and the
    /// status says why no remote program can reach the local display.
    X11 {
        /// The target hop whose session it belongs to.
        hop: HopLocation,
        /// Off, enabling, ready or failed.
        status: X11Status,
    },
    /// The attempt failed, or a live chain stopped.
    Failed(ChainFailure),
}

/// Everything one jump attempt needs.
///
/// Building the request does not open a socket and does not start a process.
/// Those happen, if at all, after [`ChainSession::connect`] and only once a
/// hop's own checks have passed.
pub struct ChainRequest {
    hosts: Vec<HostConfig>,
    target: HostId,
    auth: Vec<HopAuth>,
    size: PtySize,
    /// Keepalive and the per-hop deadline, resolved once for the whole chain (R20).
    ///
    /// One policy covers every hop: a chain to a host is one connection attempt, and a
    /// hop that probed differently from the target would make the reported behaviour
    /// impossible to explain. The target host's choice is what the caller resolves
    /// against the global defaults.
    policy: ConnectionPolicy,
    open_shell: bool,
    /// How this attempt forwards X11, chosen for this connection (R11).
    x11: X11Forwarding,
}

impl ChainRequest {
    /// A chain aimed at `target` through the saved host graph.
    #[must_use]
    pub fn new(hosts: Vec<HostConfig>, target: HostId, auth: Vec<HopAuth>, size: PtySize) -> Self {
        Self {
            hosts,
            target,
            auth,
            size,
            policy: ConnectionPolicy::default(),
            open_shell: true,
            x11: X11Forwarding::Off,
        }
    }

    /// The same chain, asking the target for X11 forwarding.
    ///
    /// The mode belongs to this attempt alone: the host's saved default only decided
    /// what the connection form started with. Everything else - trust, authentication,
    /// jumps - stays as it was, because forwarding runs on the connection the shell
    /// already authenticated.
    #[must_use]
    pub fn with_x11(mut self, mode: X11Forwarding) -> Self {
        self.x11 = mode;
        self
    }

    /// The same chain without a shell on the target.
    ///
    /// Trust and authentication are unchanged. The target reports connected once
    /// those succeed, and the chain then carries forwarding instead of a terminal.
    #[must_use]
    pub fn without_shell(mut self) -> Self {
        self.open_shell = false;
        self
    }

    /// The same request with another deadline for each hop's machine work.
    ///
    /// Only the deadline changes; the keepalive stays what the policy already said. The
    /// window uses [`Self::with_connection_policy`] when a host or the preferences
    /// chose a value.
    #[must_use]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.policy = self.policy.with_connect_timeout(timeout);
        self
    }

    /// The same chain with a resolved connection policy (R20).
    ///
    /// The keepalive and the deadline every hop uses come from this one value.
    #[must_use]
    pub fn with_connection_policy(mut self, policy: ConnectionPolicy) -> Self {
        self.policy = policy;
        self
    }
}

/// A jump attempt, from the first refusal or prompt through the target shell.
pub struct ChainSession {
    commands: UnboundedSender<Command>,
    events: UnboundedReceiver<ChainEvent>,
    trust: Arc<AnswerSlot>,
    proxy: Arc<AnswerSlot>,
    cancel: Cancel,
    size: PtySize,
    closed: bool,
    files: Option<FileLink>,
    transfers: Option<TransferLink>,
    forwards: Option<ForwardLink>,
}

impl ChainSession {
    /// Starts the chain on a background thread.
    ///
    /// The call returns immediately. A cycle or a missing hop is reported as a
    /// failure event and does not dial. A ProxyCommand is reported as a
    /// confirmation and is not started until [`ChainSession::confirm_proxy`]
    /// accepts it.
    #[must_use]
    pub fn connect(request: ChainRequest) -> Self {
        let (commands, command_rx) = unbounded_channel();
        let (events_tx, events_rx) = unbounded_channel();
        let cancel = Cancel::new();
        let trust = Arc::new(AnswerSlot::new());
        let proxy = Arc::new(AnswerSlot::new());
        let (file_requests, file_request_rx) = unbounded_channel();
        let (file_responses, file_response_rx) = unbounded_channel();
        let files = sftp::Wiring {
            requests: file_request_rx,
            responses: file_responses,
        };
        let (transfer_commands, transfer_command_rx) = unbounded_channel();
        let (transfer_events, transfer_event_rx) = unbounded_channel();
        let transfers = transfer::Wiring {
            commands: transfer_command_rx,
            events: transfer_events,
        };
        let (forward_link, forward_events, forward_gate) = forwarding::channel(commands.clone());
        // The X11 side of this attempt exists from the start: the target's handler has
        // to know it before the connection is made, because the server may open its
        // first `x11` channel the moment the request is accepted.
        let (x11_gate, x11_events) = X11Gate::channel(request.x11);
        let size = request.size;
        let open_commands = commands.clone();
        let failure_sender = events_tx.clone();
        let attempt = Attempt {
            events: events_tx,
            trust: Arc::clone(&trust),
            proxy: Arc::clone(&proxy),
            cancel: cancel.clone(),
            open_commands,
            size,
            policy: request.policy,
            open_shell: request.open_shell,
            x11: x11_gate,
        };

        thread::Builder::new()
            .name("shellune-ssh-chain".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(4)
                    .enable_all()
                    .build();
                let Ok(runtime) = runtime else {
                    let _ = failure_sender.send(ChainEvent::Failed(ChainFailure::NotDialed {
                        host_id: request.target,
                        message: "the SSH runtime could not be started. Nothing was connected and no command was run.".to_owned(),
                    }));
                    return;
                };
                runtime.block_on(run_chain(
                    request,
                    command_rx,
                    attempt,
                    files,
                    transfers,
                    forward_events,
                    forward_gate,
                    x11_events,
                ));
            })
            .expect("the SSH chain thread can be created");

        Self {
            commands,
            events: events_rx,
            trust,
            proxy,
            cancel,
            size,
            closed: false,
            files: Some(FileLink::new(file_requests, file_response_rx)),
            transfers: Some(TransferLink::new(transfer_commands, transfer_event_rx)),
            forwards: Some(forward_link),
        }
    }

    /// Takes the forwarding side of the target connection, once.
    pub fn take_forward_link(&mut self) -> Option<ForwardLink> {
        self.forwards.take()
    }

    /// Takes the browsable side of the target connection, once.
    pub fn take_file_link(&mut self) -> Option<FileLink> {
        self.files.take()
    }

    /// Takes the transfer side of the target connection, once.
    pub fn take_transfer_link(&mut self) -> Option<TransferLink> {
        self.transfers.take()
    }

    /// Answers the trust question for one hop.
    ///
    /// False means that hop is not asking: it is the wrong node, the question
    /// already ended, or the attempt was closed. A caller that stores a trust
    /// record must check the result, because a false answer was not used.
    #[must_use]
    pub fn answer_host_key(&mut self, host_id: HostId, accept: bool) -> bool {
        if self.closed {
            return false;
        }
        self.trust.answer(host_id, accept)
    }

    /// Confirms or refuses the ProxyCommand for one hop.
    ///
    /// False means the command was not started by this call. Refusing, or
    /// answering a hop that is not waiting, does not start a process.
    #[must_use]
    pub fn confirm_proxy(&mut self, host_id: HostId, allow: bool) -> bool {
        if self.closed {
            return false;
        }
        self.proxy.answer(host_id, allow)
    }

    /// Sends bytes to the target shell.
    ///
    /// # Errors
    ///
    /// Returns the reason when the chain thread is gone.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.commands
            .send(Command::Write(bytes.to_vec()))
            .map_err(|_| "the SSH connection is no longer open".to_owned())
    }

    /// Tells the target shell the window changed size.
    ///
    /// # Errors
    ///
    /// Returns the reason when the chain thread is gone.
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

    /// The next event, if one has already arrived.
    pub fn try_event(&mut self) -> Option<ChainEvent> {
        self.events.try_recv().ok()
    }

    /// Every event that arrived since the last call.
    pub fn drain(&mut self) -> Vec<ChainEvent> {
        let mut events = Vec::new();
        while let Some(event) = self.try_event() {
            events.push(event);
        }
        events
    }

    /// The size the target shell was last told about.
    #[must_use]
    pub fn size(&self) -> PtySize {
        self.size
    }

    /// Stops the attempt and every process it started.
    ///
    /// A trust or ProxyCommand question that was open stops being answerable
    /// as part of the same call.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        // The flag is published before the open question is dropped. A hop that
        // wakes because its answer disappeared must already see the attempt as
        // cancelled, or it can report an unanswered identity instead.
        self.cancel.fire();
        self.trust.close();
        self.proxy.close();
        let _ = self.commands.send(Command::Close);
    }

    /// Whether [`ChainSession::close`] was called.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Opens one session channel on the target that runs one command, for the batch
    /// runner (R19).
    ///
    /// It is the same channel the SCP copy asks for, opened by the loop that owns the
    /// target's connection, so a batch step on a jump chain uses exactly the trust and
    /// authentication the chain already decided.
    pub(crate) fn open_exec(
        &mut self,
        command: String,
    ) -> Result<oneshot::Receiver<Result<Channel<client::Msg>, ChannelFailure>>, String> {
        if self.closed {
            return Err("the chain is already closed".to_owned());
        }
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Exec { command, reply })
            .map_err(|_| "the connection is gone".to_owned())?;
        Ok(answer)
    }
}

impl Drop for ChainSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// One outstanding yes/no question.
///
/// Only the hop that installed the question can be answered. Closing the slot
/// drops the sender, so a waiting hop sees "no answer" and a later call returns
/// false instead of accepting a node that has already moved on.
struct AnswerSlot {
    current: Mutex<Option<PendingAnswer>>,
}

struct PendingAnswer {
    host_id: HostId,
    reply: oneshot::Sender<bool>,
}

impl AnswerSlot {
    fn new() -> Self {
        Self {
            current: Mutex::new(None),
        }
    }

    fn answer(&self, host_id: HostId, accept: bool) -> bool {
        let mut slot = self
            .current
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(pending) = slot.as_ref() else {
            return false;
        };
        if pending.host_id != host_id {
            return false;
        }
        let pending = slot.take().expect("the pending answer was just checked");
        pending.reply.send(accept).is_ok()
    }

    fn close(&self) {
        let mut slot = self
            .current
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *slot = None;
    }

    fn install(&self, host_id: HostId, reply: oneshot::Sender<bool>) {
        let mut slot = self
            .current
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *slot = Some(PendingAnswer { host_id, reply });
    }

    fn clear_if(&self, host_id: HostId) {
        let mut slot = self
            .current
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if slot
            .as_ref()
            .is_some_and(|pending| pending.host_id == host_id)
        {
            *slot = None;
        }
    }
}

#[derive(Debug)]
struct PlannedHop {
    location: HopLocation,
    auth: AuthMethod,
    known: KnownHostKey,
    proxy_template: Option<String>,
}

struct LiveHop {
    location: HopLocation,
    handle: client::Handle<HopHandler>,
    proxy: Option<ProxyProcess>,
    bridge: Option<tokio::task::JoinHandle<()>>,
    /// Set by this hop's handler when its own SSH connection ends.
    lost: watch::Receiver<bool>,
}

struct HopHandler {
    location: HopLocation,
    known: KnownHostKey,
    events: UnboundedSender<ChainEvent>,
    trust: Arc<AnswerSlot>,
    cancel: Cancel,
    progress: Arc<HandshakeProgress>,
    recorded: Arc<Mutex<Option<ConnectError>>>,
    gate: std::sync::Arc<forwarding::ForwardGate>,
    /// Where this hop's `x11` channels are bridged. A hop in front of the target gets
    /// a gate that refuses them.
    x11: Arc<X11Gate>,
}

impl HopHandler {
    fn record(&self, error: ConnectError) {
        if let Ok(mut slot) = self.recorded.lock() {
            *slot = Some(error);
        }
    }
}

impl Handler for HopHandler {
    type Error = ConnectError;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // The identity is known, so the machine part of this hop is over.
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
            KnownHostKey::Unknown => self.ask_trust(presented).await,
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

    /// A server opened an `x11` channel for a remote program.
    ///
    /// Only the target's gate has a display to bridge to, so a jump host's channel is
    /// refused instead of being sent to this machine's X server.
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

    /// The server accepted a channel request. The order of the answers is the order of
    /// the requests, which is how the answer to `x11-req` is recognised.
    async fn channel_success(
        &mut self,
        channel: russh::ChannelId,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        self.x11.on_channel_reply(channel.number(), true);
        Ok(())
    }

    /// The server refused a channel request, which for `x11-req` is a refusal to
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

impl HopHandler {
    async fn ask_trust(&mut self, presented: PresentedKey) -> Result<bool, ConnectError> {
        self.progress.set(HandshakeStage::WaitingForUser);
        let (sender, receiver) = oneshot::channel();
        self.trust.install(self.location.host_id, sender);
        let prompt = HostKeyPrompt {
            host: self.location.address.clone(),
            port: self.location.port,
            presented: presented.clone(),
        };
        if self
            .events
            .send(ChainEvent::HostKeyPrompt {
                hop: self.location.clone(),
                prompt,
            })
            .is_err()
        {
            self.trust.clear_if(self.location.host_id);
            self.record(ConnectError::HostKeyUndecided { presented });
            return Ok(false);
        }
        let decision = {
            let cancel = self.cancel.clone();
            tokio::select! {
                answer = receiver => answer.ok(),
                _ = cancel.cancelled() => None,
            }
        };
        self.trust.clear_if(self.location.host_id);
        self.progress.set(HandshakeStage::UserDecided);
        // No answer means the question was closed. Mark the attempt cancelled
        // before the handshake failure is classified, and do not accept the key.
        if self.cancel.is_cancelled() || decision.is_none() {
            self.cancel.fire();
            return Ok(false);
        }
        if decision == Some(false) {
            self.record(ConnectError::HostKeyRejected { presented });
            return Ok(false);
        }
        Ok(true)
    }
}

/// The channels and deadlines one attempt shares across its hops.
struct Attempt {
    events: UnboundedSender<ChainEvent>,
    trust: Arc<AnswerSlot>,
    proxy: Arc<AnswerSlot>,
    cancel: Cancel,
    open_commands: UnboundedSender<Command>,
    size: PtySize,
    /// Keepalive and deadline every hop of this attempt uses (R20).
    policy: ConnectionPolicy,
    open_shell: bool,
    /// The target's X11 forwarding, shared with its handler.
    x11: Arc<X11Gate>,
}

#[allow(clippy::too_many_arguments)]
async fn run_chain(
    request: ChainRequest,
    mut commands: UnboundedReceiver<Command>,
    attempt: Attempt,
    files: sftp::Wiring,
    transfers: transfer::Wiring,
    forwards: tokio::sync::mpsc::UnboundedSender<forwarding::ForwardEvent>,
    gate: Arc<forwarding::ForwardGate>,
    x11_events: UnboundedReceiver<X11Notice>,
) {
    let result = connect_chain(
        request,
        &mut commands,
        &attempt,
        files,
        transfers,
        forwards,
        gate,
        x11_events,
    )
    .await;
    // Whatever ended the attempt, no bridge may stay open once it is over: closing
    // stops accepting new `x11` channels before the open ones are dropped.
    attempt.x11.close();
    if let Err(error) = result {
        let _ = attempt.events.send(ChainEvent::Failed(error));
    }
}

#[allow(clippy::too_many_arguments)]
async fn connect_chain(
    request: ChainRequest,
    commands: &mut UnboundedReceiver<Command>,
    attempt: &Attempt,
    files: sftp::Wiring,
    transfers: transfer::Wiring,
    forwards: tokio::sync::mpsc::UnboundedSender<forwarding::ForwardEvent>,
    gate: Arc<forwarding::ForwardGate>,
    mut x11_events: UnboundedReceiver<X11Notice>,
) -> Result<(), ChainFailure> {
    let cancel = &attempt.cancel;
    let planned = plan_chain(&request.hosts, request.target, request.auth)?;
    let count = planned.len();
    let mut live = Vec::with_capacity(count);
    for (index, hop) in planned.into_iter().enumerate() {
        if cancel.is_cancelled() {
            let current = hop.location.clone();
            cleanup(&mut live).await;
            return Err(ChainFailure::Cancelled {
                hop: Some(Box::new(current)),
            });
        }
        let hop_gate = if index + 1 == count {
            Arc::clone(&gate)
        } else {
            forwarding::ForwardGate::reject_all()
        };
        let hop_x11 = if index + 1 == count {
            Arc::clone(&attempt.x11)
        } else {
            X11Gate::rejecting()
        };
        let opened =
            match open_hop(hop, index > 0, live.last_mut(), attempt, hop_gate, hop_x11).await {
                Ok(opened) => opened,
                Err(error) => {
                    cleanup(&mut live).await;
                    return Err(error);
                }
            };
        live.push(opened);
        if index + 1 == count {
            let result = drive_shell(
                &mut live,
                commands,
                attempt,
                files,
                transfers,
                forwards,
                gate,
                &mut x11_events,
            )
            .await;
            cleanup(&mut live).await;
            return result;
        }
    }
    cleanup(&mut live).await;
    Err(ChainFailure::NotDialed {
        host_id: request.target,
        message: "the jump chain was empty, so nothing was connected and no command was run."
            .to_owned(),
    })
}

fn plan_chain(
    hosts: &[HostConfig],
    target: HostId,
    auth: Vec<HopAuth>,
) -> Result<Vec<PlannedHop>, ChainFailure> {
    if let Some(fault) = connection_jump_fault(hosts, target) {
        return Err(ChainFailure::NotDialed {
            host_id: target,
            message: fault.message().to_owned(),
        });
    }
    let by_id: HashMap<HostId, &HostConfig> = hosts.iter().map(|host| (host.id(), host)).collect();
    let mut order = Vec::new();
    let mut stack = Vec::new();
    expand_chain(target, &by_id, &mut order, &mut stack).map_err(|message| {
        ChainFailure::NotDialed {
            host_id: target,
            message,
        }
    })?;

    let mut credentials = HashMap::new();
    for entry in auth {
        if credentials.contains_key(&entry.host_id) {
            return Err(ChainFailure::NotDialed {
                host_id: entry.host_id,
                message: format!(
                    "host {} was given two credential sets. Keep one before connecting. Nothing was connected and no command was run.",
                    entry.host_id
                ),
            });
        }
        credentials.insert(entry.host_id, entry);
    }

    let mut planned = Vec::with_capacity(order.len());
    for (index, id) in order.into_iter().enumerate() {
        let Some(host) = by_id.get(&id) else {
            return Err(ChainFailure::NotDialed {
                host_id: target,
                message: format!(
                    "host {id} is not saved, so it cannot be connected. Nothing was connected and no command was run."
                ),
            });
        };
        if !host.jumps().is_empty() && host.proxy_command().is_some() {
            return Err(ChainFailure::NotDialed {
                host_id: host.id(),
                message: format!(
                    "\"{}\" has both a jump chain and a ProxyCommand. Keep one way to reach it and remove the other before connecting. Nothing was connected and no command was run.",
                    host.name()
                ),
            });
        }
        let Some(entry) = credentials.remove(&id) else {
            return Err(ChainFailure::NotDialed {
                host_id: host.id(),
                message: format!(
                    "\"{}\" ({}) needs credentials before it can be connected. Nothing was connected and no command was run.",
                    host.name(),
                    host.target()
                ),
            });
        };
        planned.push(PlannedHop {
            location: locate(host, index),
            auth: entry.auth,
            known: entry.known,
            proxy_template: host.proxy_command().map(str::to_owned),
        });
    }
    Ok(planned)
}

fn expand_chain(
    id: HostId,
    by_id: &HashMap<HostId, &HostConfig>,
    out: &mut Vec<HostId>,
    stack: &mut Vec<HostId>,
) -> Result<(), String> {
    if out.contains(&id) {
        return Ok(());
    }
    if stack.contains(&id) {
        return Err(
            "the jump chain loops. Remove the hop that returns to an earlier host before connecting. Nothing was connected and no command was run."
                .to_owned(),
        );
    }
    let Some(host) = by_id.get(&id) else {
        return Err(format!(
            "host {id} is not saved, so it cannot be connected. Nothing was connected and no command was run."
        ));
    };
    stack.push(id);
    for hop in host.jumps() {
        expand_chain(*hop, by_id, out, stack)?;
    }
    stack.pop();
    if !out.contains(&id) {
        out.push(id);
    }
    Ok(())
}

fn locate(host: &HostConfig, index: usize) -> HopLocation {
    HopLocation {
        host_id: host.id(),
        index,
        name: host.name().to_owned(),
        address: host.address().to_owned(),
        port: host.port().get(),
        user: host.user().to_owned(),
    }
}

async fn open_hop(
    hop: PlannedHop,
    has_previous: bool,
    previous: Option<&mut LiveHop>,
    attempt: &Attempt,
    gate: Arc<forwarding::ForwardGate>,
    x11: Arc<X11Gate>,
) -> Result<LiveHop, ChainFailure> {
    let events = &attempt.events;
    let trust = &attempt.trust;
    let proxy_slot = &attempt.proxy;
    let cancel = &attempt.cancel;
    let timeout = attempt.policy.connect_timeout();
    let location = hop.location.clone();
    if events
        .send(ChainEvent::HopStarted(location.clone()))
        .is_err()
    {
        return Err(ChainFailure::Cancelled {
            hop: Some(Box::new(location)),
        });
    }
    let expanded = hop.proxy_template.as_ref().map(|template| {
        transport::expand_proxy_command(template, &location.address, location.port, &location.user)
    });
    if let Some(command) = expanded.as_deref() {
        confirm_proxy(proxy_slot, cancel, &location, command, events).await?;
    }
    if cancel.is_cancelled() {
        return Err(ChainFailure::Cancelled {
            hop: Some(Box::new(location)),
        });
    }

    let material = Material::resolve(hop.auth)
        .await
        .map_err(|error| ChainFailure::Hop {
            hop: Box::new(location.clone()),
            error: Box::new(error),
        })?;

    let opened = match open_ingress(
        &location,
        expanded.as_deref(),
        has_previous,
        previous,
        cancel,
        timeout,
    )
    .await
    {
        Ok(opened) => opened,
        Err(error) => return Err(error),
    };

    let recorded = Arc::new(Mutex::new(None));
    let progress = Arc::new(HandshakeProgress::default());
    let lost = gate.subscribe_lost();
    let handler = HopHandler {
        location: location.clone(),
        known: hop.known,
        events: events.clone(),
        trust: Arc::clone(trust),
        cancel: cancel.clone(),
        progress: Arc::clone(&progress),
        recorded: Arc::clone(&recorded),
        gate,
        x11,
    };
    // The same mapping a direct session uses, so a hop and a direct connection cannot
    // probe differently (R20).
    let config = Arc::new(client_config(&attempt.policy));
    let stream = opened.stream;
    let dial = finish_dial(
        client::connect_stream(config, stream, handler),
        &progress,
        timeout,
    );
    tokio::pin!(dial);
    let outcome = tokio::select! {
        result = dial.as_mut() => DialEnd::Finished(result),
        _ = cancel.cancelled() => DialEnd::Cancelled,
    };
    let mut handle = match outcome {
        DialEnd::Cancelled => {
            drop_ingress(opened.proxy, opened.bridge).await;
            return Err(ChainFailure::Cancelled {
                hop: Some(Box::new(location)),
            });
        }
        DialEnd::Finished(Ok(handle)) => handle,
        DialEnd::Finished(Err(failure)) => {
            let error = match failure {
                DialFailure::Timeout => ConnectError::TimedOut {
                    target: location.target(),
                },
                DialFailure::Attempt(error) => take_recorded(&recorded).unwrap_or(error),
            };
            drop_ingress(opened.proxy, opened.bridge).await;
            if cancel.is_cancelled() {
                return Err(ChainFailure::Cancelled {
                    hop: Some(Box::new(location)),
                });
            }
            return Err(ChainFailure::Hop {
                hop: Box::new(location),
                error: Box::new(error),
            });
        }
    };
    if cancel.is_cancelled() {
        let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
        drop_ingress(opened.proxy, opened.bridge).await;
        return Err(ChainFailure::Cancelled {
            hop: Some(Box::new(location)),
        });
    }
    if let Err(error) = authenticate(&mut handle, &location.user, material).await {
        let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
        drop_ingress(opened.proxy, opened.bridge).await;
        if cancel.is_cancelled() {
            return Err(ChainFailure::Cancelled {
                hop: Some(Box::new(location)),
            });
        }
        return Err(ChainFailure::Hop {
            hop: Box::new(location),
            error: Box::new(error),
        });
    }
    if events.send(ChainEvent::HopReady(location.clone())).is_err() {
        let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
        drop_ingress(opened.proxy, opened.bridge).await;
        return Err(ChainFailure::Cancelled {
            hop: Some(Box::new(location)),
        });
    }
    Ok(LiveHop {
        location,
        handle,
        proxy: opened.proxy,
        bridge: opened.bridge,
        lost,
    })
}

enum DialEnd<T> {
    Finished(T),
    Cancelled,
}

struct OpenedIngress {
    stream: IoStream,
    proxy: Option<ProxyProcess>,
    bridge: Option<tokio::task::JoinHandle<()>>,
}

async fn drop_ingress(proxy: Option<ProxyProcess>, bridge: Option<tokio::task::JoinHandle<()>>) {
    if let Some(bridge) = bridge {
        bridge.abort();
    }
    if let Some(proxy) = proxy {
        proxy.shutdown().await;
    }
}

async fn confirm_proxy(
    slot: &AnswerSlot,
    cancel: &Cancel,
    hop: &HopLocation,
    command: &str,
    events: &UnboundedSender<ChainEvent>,
) -> Result<(), ChainFailure> {
    if cancel.is_cancelled() {
        return Err(ChainFailure::Cancelled {
            hop: Some(Box::new(hop.clone())),
        });
    }
    let (sender, receiver) = oneshot::channel();
    slot.install(hop.host_id, sender);
    if events
        .send(ChainEvent::ProxyConfirmation {
            hop: hop.clone(),
            command: command.to_owned(),
        })
        .is_err()
    {
        slot.clear_if(hop.host_id);
        return Err(ChainFailure::Cancelled {
            hop: Some(Box::new(hop.clone())),
        });
    }
    let decision = tokio::select! {
        answer = receiver => answer.ok(),
        _ = cancel.cancelled() => None,
    };
    slot.clear_if(hop.host_id);
    if cancel.is_cancelled() || decision.is_none() {
        return Err(ChainFailure::Cancelled {
            hop: Some(Box::new(hop.clone())),
        });
    }
    if decision == Some(false) {
        return Err(ChainFailure::ProxyNotConfirmed {
            hop: Box::new(hop.clone()),
            command: command.to_owned(),
        });
    }
    // Confirmed, but a close that won the race must still not start the process.
    if cancel.is_cancelled() {
        return Err(ChainFailure::Cancelled {
            hop: Some(Box::new(hop.clone())),
        });
    }
    Ok(())
}

async fn open_ingress(
    location: &HopLocation,
    proxy_command: Option<&str>,
    has_previous: bool,
    previous: Option<&mut LiveHop>,
    cancel: &Cancel,
    timeout: Duration,
) -> Result<OpenedIngress, ChainFailure> {
    if cancel.is_cancelled() {
        return Err(ChainFailure::Cancelled {
            hop: Some(Box::new(location.clone())),
        });
    }
    if let Some(command) = proxy_command {
        let (process, stream) =
            transport::spawn_proxy(command)
                .await
                .map_err(|error| ChainFailure::Hop {
                    hop: Box::new(location.clone()),
                    error: Box::new(error),
                })?;
        if cancel.is_cancelled() {
            process.shutdown().await;
            return Err(ChainFailure::Cancelled {
                hop: Some(Box::new(location.clone())),
            });
        }
        return Ok(OpenedIngress {
            stream,
            proxy: Some(process),
            bridge: None,
        });
    }
    if has_previous {
        let Some(previous) = previous else {
            return Err(ChainFailure::Hop {
                hop: Box::new(location.clone()),
                error: Box::new(ConnectError::Unreachable {
                    reason: "the previous hop is gone, so this hop was not dialled".to_owned(),
                }),
            });
        };
        let open = previous.handle.channel_open_direct_tcpip(
            location.address.clone(),
            u32::from(location.port),
            "127.0.0.1",
            0,
        );
        tokio::pin!(open);
        let channel = tokio::select! {
            result = open.as_mut() => result,
            _ = tokio::time::sleep(timeout) => {
                return Err(ChainFailure::Hop {
                    hop: Box::new(location.clone()),
                    error: Box::new(ConnectError::TimedOut { target: location.target() }),
                });
            }
            _ = cancel.cancelled() => {
                return Err(ChainFailure::Cancelled { hop: Some(Box::new(location.clone())) });
            }
        };
        let channel = channel.map_err(|error| ChainFailure::Hop {
            hop: Box::new(location.clone()),
            error: Box::new(ConnectError::Unreachable {
                reason: format!(
                    "the previous hop could not open a channel to {}: {error}",
                    location.target()
                ),
            }),
        })?;
        let (stream, bridge) = transport::bridge_channel(channel, cancel.clone());
        return Ok(OpenedIngress {
            stream,
            proxy: None,
            bridge: Some(bridge),
        });
    }

    let connect = tokio::net::TcpStream::connect((location.address.as_str(), location.port));
    tokio::pin!(connect);
    let stream = tokio::select! {
        result = connect.as_mut() => result,
        _ = tokio::time::sleep(timeout) => {
            return Err(ChainFailure::Hop {
                hop: Box::new(location.clone()),
                error: Box::new(ConnectError::TimedOut { target: location.target() }),
            });
        }
        _ = cancel.cancelled() => {
            return Err(ChainFailure::Cancelled { hop: Some(Box::new(location.clone())) });
        }
    };
    let stream = stream.map_err(|error| ChainFailure::Hop {
        hop: Box::new(location.clone()),
        error: Box::new(transport::io_connect_error(&location.target(), error)),
    })?;
    let _ = stream.set_nodelay(true);
    let (stream, bridge) = transport::bridge_tcp(stream, cancel.clone());
    Ok(OpenedIngress {
        stream,
        proxy: None,
        bridge: Some(bridge),
    })
}

fn take_recorded(recorded: &Mutex<Option<ConnectError>>) -> Option<ConnectError> {
    recorded.lock().ok().and_then(|mut slot| slot.take())
}

#[allow(clippy::too_many_arguments)]
async fn drive_shell(
    live: &mut [LiveHop],
    commands: &mut UnboundedReceiver<Command>,
    attempt: &Attempt,
    files: sftp::Wiring,
    transfers: transfer::Wiring,
    forward_events: tokio::sync::mpsc::UnboundedSender<forwarding::ForwardEvent>,
    gate: Arc<forwarding::ForwardGate>,
    x11_events: &mut UnboundedReceiver<X11Notice>,
) -> Result<(), ChainFailure> {
    let events = &attempt.events;
    let cancel = &attempt.cancel;
    let size = attempt.size;
    let open_commands = attempt.open_commands.clone();
    let last = live.len() - 1;
    let (jumps, target_slot) = live.split_at_mut(last);
    let target = &mut target_slot[0];
    let location = target.location.clone();
    let handle = &mut target.handle;
    let mut carrier_lost = gate.subscribe_lost();
    let mut forwards = forwarding::ForwardLoop::new(forward_events, gate, open_commands.clone());
    let report_x11 = |status: X11Status| {
        let _ = events.send(ChainEvent::X11 {
            hop: location.clone(),
            status,
        });
    };

    let shell = if attempt.open_shell {
        // The local display is checked before the request is sent, so a machine with
        // no X server says so at once instead of leaving a remote program to fail on
        // its own. Either way the shell below is opened as it always was.
        if !attempt.x11.mode().is_enabled() {
            // A session that forwards nothing says so: the bar states the mode rather
            // than leaving the reader to guess whether a display was requested.
            report_x11(X11Status::Off);
        }
        let prepared = if attempt.x11.mode().is_enabled() {
            // A display that is not there, an authority file without a cookie and a
            // display that cannot issue a restricted cookie are reported before the
            // request is sent, and never stop the shell below.
            match x11::arm(attempt.x11.mode()).await {
                Ok(prepared) => Some(prepared),
                Err(failure) => {
                    report_x11(X11Status::Failed {
                        reason: failure.to_string(),
                        next_step: failure.next_step(),
                    });
                    None
                }
            }
        } else {
            None
        };
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|error| ChainFailure::Hop {
                hop: Box::new(location.clone()),
                error: Box::new(ConnectError::PtyRejected {
                    reason: error.to_string(),
                }),
            })?;
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
            .map_err(|error| ChainFailure::Hop {
                hop: Box::new(location.clone()),
                error: Box::new(ConnectError::PtyRejected {
                    reason: error.to_string(),
                }),
            })?;
        // The request has to precede the shell: a server sets `DISPLAY` and writes the
        // authority file while it prepares the session's environment, which happens
        // before the shell starts.
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
                    attempt.x11.announce(&prepared);
                    true
                }
                Err(error) => {
                    let failure = x11::X11Failure::Lost {
                        detail: error.to_string(),
                    };
                    report_x11(X11Status::Failed {
                        reason: failure.to_string(),
                        next_step: failure.next_step(),
                    });
                    false
                }
            }
        } else {
            false
        };
        // The channel's answers are counted from here: the pseudo-terminal's, the X11
        // request's when one was sent, and then the shell's.
        attempt.x11.shell_requests(channel.id().number(), requested);
        channel
            .request_shell(true)
            .await
            .map_err(|error| ChainFailure::Hop {
                hop: Box::new(location.clone()),
                error: Box::new(ConnectError::PtyRejected {
                    reason: error.to_string(),
                }),
            })?;
        Some(channel.split())
    } else {
        None
    };
    if events
        .send(ChainEvent::Connected {
            hop: location.clone(),
        })
        .is_err()
    {
        forwards.shutdown(handle).await;
        return Err(ChainFailure::Cancelled {
            hop: Some(Box::new(location)),
        });
    }
    tokio::spawn(sftp::serve(files, open_commands.clone()));
    tokio::spawn(transfer::serve(transfers, open_commands));

    if let Some((mut read_half, write_half)) = shell {
        let mut exit_code: Option<u32> = None;
        let mut x11_open = true;
        loop {
            tokio::select! {
                biased;
                index = hop_ended(jumps) => {
                    forwards.shutdown(handle).await;
                    return Err(hop_disconnected(jumps[index].location.clone()));
                }
                changed = carrier_lost.changed() => {
                    if changed.is_err() || *carrier_lost.borrow() {
                        // The target carrier also drops when a jump in front of
                        // it drops. Name that jump when it has reported, or
                        // reports before the wait ends.
                        let failure = blame_closed(jumps, &location).await;
                        forwards.shutdown(handle).await;
                        return Err(failure);
                    }
                }
                _ = cancel.cancelled() => {
                    let _ = write_half.eof().await;
                    let _ = write_half.close().await;
                    forwards.shutdown(handle).await;
                    let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
                    return Ok(());
                }
                notice = x11_events.recv(), if x11_open => {
                    match notice {
                        Some(X11Notice::Status(status)) => {
                            let _ = events.send(ChainEvent::X11 {
                                hop: location.clone(),
                                status,
                            });
                        }
                        // A refusal is answered with what the target says about its own
                        // xauth, so the two server-side causes can be told apart.
                        Some(X11Notice::Refused) => {
                            let status = x11::refusal_status(handle).await;
                            let _ = events.send(ChainEvent::X11 {
                                hop: location.clone(),
                                status,
                            });
                        }
                        None => x11_open = false,
                    }
                }
                command = commands.recv() => {
                    let Some(command) = command else {
                        let _ = write_half.eof().await;
                        let _ = write_half.close().await;
                        forwards.shutdown(handle).await;
                        let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
                        return Ok(());
                    };
                    let Some(command) = forwards.accept_command(handle, command).await else {
                        continue;
                    };
                    match command {
                        Command::Close => {
                            let _ = write_half.eof().await;
                            let _ = write_half.close().await;
                            forwards.shutdown(handle).await;
                            let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
                            return Ok(());
                        }
                        Command::Write(bytes) => {
                            if let Err(error) = write_half.data_bytes(bytes).await {
                                forwards.shutdown(handle).await;
                                return Err(ChainFailure::Hop {
                                    hop: Box::new(location.clone()),
                                    error: Box::new(ConnectError::Disconnected { reason: error.to_string() }),
                                });
                            }
                        }
                        Command::Sftp(reply) => {
                            let _ = reply.send(open_sftp_channel(handle).await);
                        }
                        Command::Exec { command, reply } => {
                            let _ = reply.send(open_exec_channel(handle, &command).await);
                        }
                        Command::Resize(next) => {
                            if let Err(error) = write_half.window_change(next.cols, next.rows, 0, 0).await {
                                forwards.shutdown(handle).await;
                                return Err(ChainFailure::Hop {
                                    hop: Box::new(location.clone()),
                                    error: Box::new(ConnectError::Disconnected { reason: error.to_string() }),
                                });
                            }
                        }
                        Command::Forward(_) | Command::DirectTcp { .. } => {}
                    }
                }
                message = read_half.wait() => match message {
                    Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                        if events.send(ChainEvent::Data(data.to_vec())).is_err() {
                            forwards.shutdown(handle).await;
                            return Ok(());
                        }
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => {
                        exit_code = Some(exit_status);
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                        // A jump that died looks like the shell closing. Prefer the
                        // hop that actually dropped when it says so quickly.
                        if exit_code.is_none()
                            && let Some(index) = jump_died(jumps).await
                        {
                            forwards.shutdown(handle).await;
                            return Err(hop_disconnected(jumps[index].location.clone()));
                        }
                        let _ = events.send(ChainEvent::Exited { code: exit_code });
                        forwards.shutdown(handle).await;
                        let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
                        return Ok(());
                    }
                    Some(_) => {}
                },
            }
        }
    }

    let mut x11_open = true;
    loop {
        tokio::select! {
            biased;
            index = hop_ended(jumps) => {
                forwards.shutdown(handle).await;
                return Err(hop_disconnected(jumps[index].location.clone()));
            }
            changed = carrier_lost.changed() => {
                if changed.is_err() || *carrier_lost.borrow() {
                    let failure = blame_closed(jumps, &location).await;
                    forwards.shutdown(handle).await;
                    return Err(failure);
                }
            }
            _ = cancel.cancelled() => {
                forwards.shutdown(handle).await;
                let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
                return Ok(());
            }
            notice = x11_events.recv(), if x11_open => {
                match notice {
                    Some(X11Notice::Status(status)) => {
                        let _ = events.send(ChainEvent::X11 {
                            hop: location.clone(),
                            status,
                        });
                    }
                    // A refusal is answered with what the target says about its own
                    // xauth, so the two server-side causes can be told apart.
                    Some(X11Notice::Refused) => {
                        let status = x11::refusal_status(handle).await;
                        let _ = events.send(ChainEvent::X11 {
                            hop: location.clone(),
                            status,
                        });
                    }
                    None => x11_open = false,
                }
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    forwards.shutdown(handle).await;
                    let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
                    return Ok(());
                };
                let Some(command) = forwards.accept_command(handle, command).await else {
                    continue;
                };
                match command {
                    Command::Close => {
                        forwards.shutdown(handle).await;
                        let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
                        return Ok(());
                    }
                    Command::Sftp(reply) => {
                        let _ = reply.send(open_sftp_channel(handle).await);
                    }
                    Command::Exec { command, reply } => {
                        let _ = reply.send(open_exec_channel(handle, &command).await);
                    }
                    Command::Write(_) | Command::Resize(_) | Command::Forward(_) | Command::DirectTcp { .. } => {}
                }
            }
        }
    }
}

fn hop_ended(hops: &mut [LiveHop]) -> impl Future<Output = usize> + '_ {
    std::future::poll_fn(move |cx| poll_dead_hop(hops, cx))
}

fn poll_dead_hop(hops: &mut [LiveHop], cx: &mut Context<'_>) -> Poll<usize> {
    for (index, hop) in hops.iter_mut().enumerate() {
        if *hop.lost.borrow() || Pin::new(&mut hop.handle).poll(cx).is_ready() {
            return Poll::Ready(index);
        }
    }
    Poll::Pending
}

fn hop_disconnected(hop: HopLocation) -> ChainFailure {
    ChainFailure::Hop {
        hop: Box::new(hop),
        error: Box::new(ConnectError::Disconnected {
            reason: "the connection to this hop closed".to_owned(),
        }),
    }
}

/// Names an earlier hop when the target's carrier died because that hop did.
async fn blame_closed(jumps: &mut [LiveHop], target: &HopLocation) -> ChainFailure {
    match jump_died(jumps).await {
        Some(index) => hop_disconnected(jumps[index].location.clone()),
        None => hop_disconnected(target.clone()),
    }
}

fn lost_hop(hops: &mut [LiveHop]) -> Option<usize> {
    let waker = std::task::Waker::noop();
    let mut context = Context::from_waker(waker);
    for (index, hop) in hops.iter_mut().enumerate() {
        if *hop.lost.borrow() || Pin::new(&mut hop.handle).poll(&mut context).is_ready() {
            return Some(index);
        }
    }
    None
}

async fn jump_died(hops: &mut [LiveHop]) -> Option<usize> {
    if hops.is_empty() {
        return None;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        if let Some(index) = lost_hop(hops) {
            return Some(index);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn cleanup(live: &mut Vec<LiveHop>) {
    while let Some(hop) = live.pop() {
        release_hop(hop).await;
    }
}

async fn release_hop(mut hop: LiveHop) {
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        hop.handle.disconnect(Disconnect::ByApplication, "", ""),
    )
    .await;
    if let Some(bridge) = hop.bridge.take() {
        bridge.abort();
    }
    if let Some(proxy) = hop.proxy.take() {
        proxy.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hop_is_dialled_with_the_policy_the_chain_was_given() {
        let bastion = host("bastion", "10.0.0.1", 22);
        let mut web = host("web", "10.0.0.2", 22);
        web.set_jumps([bastion.id()]);
        let own = shellune_core::HostConnection::new(Some(50), Some(4), Some(11)).expect("valid");
        let policy = shellune_core::ConnectionDefaults::default().policy_for(Some(own));
        let request = ChainRequest::new(
            vec![bastion.clone(), web.clone()],
            web.id(),
            vec![password(bastion.id()), password(web.id())],
            PtySize::default(),
        )
        .with_connection_policy(policy);
        assert_eq!(request.policy, policy);

        // The configuration a hop is opened with comes from that policy, through the
        // same function a direct session uses: one mapping, so the two cannot drift.
        let config = client_config(&request.policy);
        assert_eq!(config.keepalive_interval, Some(Duration::from_secs(50)));
        assert_eq!(config.keepalive_max, 4);
        assert_eq!(config.inactivity_timeout, None);
        assert_eq!(request.policy.connect_timeout(), Duration::from_secs(11));

        let deadline_only = request.with_connect_timeout(Duration::from_secs(2));
        assert_eq!(
            deadline_only.policy.connect_timeout(),
            Duration::from_secs(2)
        );
        assert_eq!(
            client_config(&deadline_only.policy).keepalive_interval,
            Some(Duration::from_secs(50)),
            "a shorter deadline does not silently change the keepalive"
        );
    }

    fn host(name: &str, address: &str, port: u16) -> HostConfig {
        HostConfig::new(name, address, port, "deploy").expect("valid host")
    }

    fn password(host_id: HostId) -> HopAuth {
        HopAuth::new(
            host_id,
            AuthMethod::password("hunter2"),
            KnownHostKey::Unknown,
        )
    }

    #[test]
    fn the_chain_is_the_jumps_in_order_then_the_target() {
        let gateway = host("gateway", "10.0.0.1", 22);
        let mut bastion = host("bastion", "10.0.0.2", 22);
        bastion.set_jumps([gateway.id()]);
        let mut web = host("web", "10.0.0.3", 2222);
        web.set_jumps([bastion.id()]);
        let planned = plan_chain(
            &[gateway.clone(), bastion.clone(), web.clone()],
            web.id(),
            vec![
                password(gateway.id()),
                password(bastion.id()),
                password(web.id()),
            ],
        )
        .expect("the chain resolves");
        let names: Vec<&str> = planned
            .iter()
            .map(|hop| hop.location.name.as_str())
            .collect();
        assert_eq!(names, ["gateway", "bastion", "web"]);
        assert_eq!(planned[2].location.port, 2222);
        assert_eq!(planned[2].location.address, "10.0.0.3");
        assert!(planned.iter().all(|hop| hop.proxy_template.is_none()));
    }

    #[test]
    fn a_repeated_jump_is_dialled_once() {
        let bastion = host("bastion", "10.0.0.2", 22);
        let mut web = host("web", "10.0.0.3", 22);
        web.set_jumps([bastion.id(), bastion.id()]);
        let planned = plan_chain(
            &[bastion.clone(), web.clone()],
            web.id(),
            vec![password(bastion.id()), password(web.id())],
        )
        .expect("duplicates collapse");
        assert_eq!(planned.len(), 2);
        assert_eq!(planned[0].location.host_id, bastion.id());
        assert_eq!(planned[1].location.host_id, web.id());
    }

    #[test]
    fn a_cycle_or_a_missing_hop_is_refused_before_any_credentials_are_required() {
        let mut gateway = host("gateway", "10.0.0.1", 22);
        let mut bastion = host("bastion", "10.0.0.2", 22);
        gateway.set_jumps([bastion.id()]);
        bastion.set_jumps([gateway.id()]);
        let error = plan_chain(
            &[gateway.clone(), bastion.clone()],
            gateway.id(),
            Vec::new(),
        )
        .expect_err("a loop is not a chain");
        let ChainFailure::NotDialed { message, .. } = error else {
            panic!("expected a pre-dial refusal, got {error:?}");
        };
        assert!(message.contains("loops"), "{message}");
        assert!(message.contains("Nothing was connected"), "{message}");
        assert!(!message.contains("hunter2"), "{message}");

        let missing = HostId::new();
        let mut web = host("web", "10.0.0.3", 22);
        web.set_jumps([missing]);
        let error = plan_chain(&[web.clone()], web.id(), Vec::new()).expect_err("missing hop");
        let ChainFailure::NotDialed { message, .. } = error else {
            panic!("expected a pre-dial refusal, got {error:?}");
        };
        assert!(message.contains("not saved"), "{message}");
        assert!(message.contains(&missing.to_string()), "{message}");
    }

    #[test]
    fn a_host_with_both_a_jump_and_a_proxy_command_is_not_dialled() {
        let bastion = host("bastion", "10.0.0.2", 22);
        let mut web = host("web", "10.0.0.3", 22);
        web.set_jumps([bastion.id()]);
        web.set_proxy_command(Some("nc %h %p"));
        let error = plan_chain(
            &[bastion.clone(), web.clone()],
            web.id(),
            vec![password(bastion.id()), password(web.id())],
        )
        .expect_err("both ways is not a chain");
        let message = error.to_string();
        assert!(message.contains("ProxyCommand"), "{message}");
        assert!(message.contains("no command was run"), "{message}");
        assert!(!message.contains("hunter2"), "{message}");
    }

    #[test]
    fn a_hop_without_credentials_is_named_and_not_dialled() {
        let bastion = host("bastion", "10.0.0.2", 22);
        let mut web = host("web", "10.0.0.3", 22);
        web.set_jumps([bastion.id()]);
        let error = plan_chain(
            &[bastion.clone(), web.clone()],
            web.id(),
            vec![password(web.id())],
        )
        .expect_err("bastion has no credentials");
        let message = error.to_string();
        assert!(message.contains("bastion"), "{message}");
        assert!(message.contains("needs credentials"), "{message}");
        assert!(message.contains("Nothing was connected"), "{message}");
    }

    #[test]
    fn a_trust_answer_reaches_only_the_hop_that_is_asking() {
        let slot = AnswerSlot::new();
        let asking = HostId::new();
        let other = HostId::new();
        assert!(
            !slot.answer(asking, true),
            "nothing is asking yet, so the answer is not used"
        );
        let (sender, mut receiver) = oneshot::channel();
        slot.install(asking, sender);
        assert!(
            !slot.answer(other, true),
            "a different hop must not accept this question"
        );
        assert!(slot.answer(asking, true));
        assert_eq!(receiver.try_recv().ok(), Some(true));
        let (sender, mut receiver) = oneshot::channel();
        slot.install(asking, sender);
        slot.close();
        assert!(
            !slot.answer(asking, true),
            "a closed hop does not accept a late answer"
        );
        assert!(receiver.try_recv().is_err());
    }
}
