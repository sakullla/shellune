//! Local, remote and dynamic forwarding on one authenticated SSH connection (R11).
//!
//! A rule becomes active only after the local socket is bound or the server accepts
//! `tcpip-forward`. Stopping drops that socket and, for a remote rule, sends
//! `cancel-tcpip-forward`. An inbound `forwarded-tcpip` channel is accepted only when
//! it matches a rule that is still active; every other channel is rejected. The
//! default russh accept is not used.
//!
//! A failure to bind, a refusal from the server, or a target that cannot be reached
//! is reported on that rule. It does not close the shell or SFTP channels that share
//! the connection. When the connection itself ends, every listener this connection
//! owned is stopped and the link reports that, so a caller cannot keep showing
//! "listening" for a socket that is gone.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use russh::client::{self, ChannelOpenHandle};
use russh::{Channel, ChannelOpenFailure};
use shellune_core::{ForwardId, ForwardKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use crate::session::Command;

/// How long a start may wait for the server or the configured target.
const OPEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// What the window asks the connection to do with one rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardCommand {
    /// Bind or request the remote listen described by the spec.
    Start(ForwardSpec),
    /// Drop the listener for this rule. The saved spec is the caller's.
    Stop {
        /// The rule to stop.
        id: ForwardId,
    },
}

/// One start. The generation is the caller's, so a late event from an older start
/// can be ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardSpec {
    /// The saved rule.
    pub id: ForwardId,
    /// Which start this is.
    pub generation: u64,
    /// Local, remote or dynamic.
    pub kind: ForwardKind,
    /// Address to bind, or the address the server should listen on.
    pub listen_host: String,
    /// Port to bind, or the port the server should listen on.
    pub listen_port: u16,
    /// Fixed target host. `None` for a dynamic rule.
    pub target_host: Option<String>,
    /// Fixed target port. `None` for a dynamic rule.
    pub target_port: Option<u16>,
}

/// Why a rule did not become, or did not stay, a usable listener.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardFailure {
    /// The local port is already taken.
    PortInUse {
        /// Address that could not be bound.
        host: String,
        /// Port that could not be bound.
        port: u16,
    },
    /// The process may not bind that address.
    PermissionDenied {
        /// Address that was refused.
        host: String,
        /// Port that was refused.
        port: u16,
    },
    /// The address is not one this machine can listen on.
    InvalidAddress {
        /// Address that was rejected.
        host: String,
        /// Port that was rejected.
        port: u16,
        /// What the operating system said.
        detail: String,
    },
    /// The server refused `direct-tcpip` or `tcpip-forward`.
    ServerRefused {
        /// The server's reason, without credentials.
        detail: String,
    },
    /// The configured target did not accept a connection.
    TargetUnreachable {
        /// Target host.
        host: String,
        /// Target port.
        port: u16,
        /// What the connect attempt said.
        detail: String,
    },
    /// The SSH connection that carried the rule is gone.
    CarrierLost,
    /// Something else, named.
    Other {
        /// The reason.
        detail: String,
    },
}

impl std::fmt::Display for ForwardFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PortInUse { host, port } => write!(
                formatter,
                "port {port} on {host} is already in use. The rule was not started."
            ),
            Self::PermissionDenied { host, port } => write!(
                formatter,
                "binding {host}:{port} was refused: permission denied. The rule was not started."
            ),
            Self::InvalidAddress { host, port, detail } => write!(
                formatter,
                "cannot listen on {host}:{port}: {detail}. The rule was not started."
            ),
            Self::ServerRefused { detail } => write!(
                formatter,
                "the server refused forwarding: {detail}. The rule was not started."
            ),
            Self::TargetUnreachable { host, port, detail } => write!(
                formatter,
                "the target {host}:{port} could not be reached: {detail}"
            ),
            Self::CarrierLost => write!(
                formatter,
                "the connection closed, so this forward is no longer listening."
            ),
            Self::Other { detail } => write!(formatter, "{detail}"),
        }
    }
}

/// What the connection reports about a rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardEvent {
    /// The listener is really up. `bound_port` is the port that was bound.
    Listening {
        /// The rule.
        id: ForwardId,
        /// The start this belongs to.
        generation: u64,
        /// Address that is listening.
        host: String,
        /// Port that is listening.
        port: u16,
    },
    /// The start failed. Nothing of this start is listening.
    Failed {
        /// The rule.
        id: ForwardId,
        /// The start this belongs to.
        generation: u64,
        /// Why.
        failure: ForwardFailure,
    },
    /// One connection through a listener that is still up failed.
    AttemptFailed {
        /// The rule.
        id: ForwardId,
        /// The start this belongs to.
        generation: u64,
        /// Why that connection failed.
        failure: ForwardFailure,
    },
    /// The user stopped the rule, or a replacement start retired it.
    Stopped {
        /// The rule.
        id: ForwardId,
        /// The start that was stopped.
        generation: u64,
    },
    /// The connection is gone. No rule on this link is listening any more.
    CarrierLost,
}

/// The application's side of one connection's forwards.
pub struct ForwardLink {
    commands: UnboundedSender<Command>,
    events: UnboundedReceiver<ForwardEvent>,
    closed: bool,
    events_closed: bool,
}

impl ForwardLink {
    pub(crate) fn new(
        commands: UnboundedSender<Command>,
        events: UnboundedReceiver<ForwardEvent>,
    ) -> Self {
        Self {
            commands,
            events,
            closed: false,
            events_closed: false,
        }
    }

    /// Asks the connection to start or stop a rule.
    ///
    /// False means the connection is already gone, so nothing was started.
    pub fn send(&mut self, command: ForwardCommand) -> bool {
        if self.closed {
            return false;
        }
        if self.commands.send(Command::Forward(command)).is_err() {
            self.closed = true;
            return false;
        }
        true
    }

    /// Events waiting since the last call.
    pub fn drain(&mut self) -> Vec<ForwardEvent> {
        let mut events = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => events.push(event),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    self.events_closed = true;
                    break;
                }
            }
        }
        events
    }

    /// Whether a later event can still arrive.
    #[must_use]
    pub fn events_open(&self) -> bool {
        !self.events_closed
    }
}

/// Shared record of the remote rules whose inbound channels may be accepted.
pub(crate) struct ForwardGate {
    inner: Mutex<GateInner>,
    events: UnboundedSender<ForwardEvent>,
    /// Becomes true when the SSH connection that owns this gate is gone.
    lost: watch::Sender<bool>,
}

struct GateInner {
    rules: Vec<InboundRule>,
    tasks: Vec<(ForwardId, JoinHandle<()>)>,
}

/// One remote listen the handler may accept a channel for.
#[derive(Clone)]
pub(crate) struct InboundRule {
    /// The rule.
    pub id: ForwardId,
    /// The start.
    pub generation: u64,
    /// Address the server was asked to listen on.
    pub listen_host: String,
    /// Port the server is listening on.
    pub listen_port: u16,
    /// Local target host.
    pub target_host: String,
    /// Local target port.
    pub target_port: u16,
    alive: Arc<AtomicBool>,
}

impl ForwardGate {
    pub(crate) fn new(events: UnboundedSender<ForwardEvent>) -> Self {
        let (lost, _) = watch::channel(false);
        Self {
            inner: Mutex::new(GateInner {
                rules: Vec::new(),
                tasks: Vec::new(),
            }),
            events,
            lost,
        }
    }

    /// A gate that accepts nothing. Jump hops use one so an inbound channel aimed
    /// at an intermediate server is rejected instead of russh's default accept.
    pub(crate) fn reject_all() -> Arc<Self> {
        let (events, _ignored) = unbounded_channel();
        Arc::new(Self::new(events))
    }

    /// Notifies the session loop that this connection can no longer carry a rule.
    pub(crate) fn mark_lost(&self) {
        let _ = self.lost.send(true);
    }

    /// Wakes when [`Self::mark_lost`] runs.
    pub(crate) fn subscribe_lost(&self) -> watch::Receiver<bool> {
        self.lost.subscribe()
    }

    fn install(&self, rule: InboundRule) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.rules.retain(|saved| saved.id != rule.id);
        inner.rules.push(rule);
    }

    fn take_tasks(&self, id: Option<ForwardId>) -> Vec<JoinHandle<()>> {
        let Ok(mut inner) = self.inner.lock() else {
            return Vec::new();
        };
        let (matched, rest): (Vec<_>, Vec<_>) = inner
            .tasks
            .drain(..)
            .partition(|(task_id, _)| id.is_none_or(|id| *task_id == id));
        if id.is_some() {
            inner.tasks = rest;
        }
        if let Some(id) = id {
            for rule in &inner.rules {
                if rule.id == id {
                    rule.alive.store(false, Ordering::SeqCst);
                }
            }
            inner.rules.retain(|rule| rule.id != id);
        } else {
            for rule in &inner.rules {
                rule.alive.store(false, Ordering::SeqCst);
            }
            inner.rules.clear();
        }
        matched.into_iter().map(|(_, handle)| handle).collect()
    }

    fn track(&self, id: ForwardId, handle: JoinHandle<()>) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.tasks.push((id, handle));
        }
    }

    fn lookup(&self, host: &str, port: u32) -> Option<InboundRule> {
        let Ok(inner) = self.inner.lock() else {
            return None;
        };
        select_inbound(&inner.rules, host, port).cloned()
    }
}

/// The listeners owned by one connection.
pub(crate) struct ForwardLoop {
    events: UnboundedSender<ForwardEvent>,
    gate: Arc<ForwardGate>,
    open: UnboundedSender<Command>,
    jobs: HashMap<ForwardId, Job>,
}

struct Job {
    generation: u64,
    remote: Option<(String, u16)>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
    bridges: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Job {
    fn shutdown(self) {
        let _ = self.stop.send(true);
        self.task.abort();
        if let Ok(mut bridges) = self.bridges.lock() {
            for bridge in bridges.drain(..) {
                bridge.abort();
            }
        }
    }
}

impl ForwardLoop {
    pub(crate) fn new(
        events: UnboundedSender<ForwardEvent>,
        gate: Arc<ForwardGate>,
        open: UnboundedSender<Command>,
    ) -> Self {
        Self {
            events,
            gate,
            open,
            jobs: HashMap::new(),
        }
    }

    /// Applies one command from the window or from a listener task.
    ///
    /// Returns the command when it belongs to the shell instead.
    pub(crate) async fn accept_command<H: client::Handler>(
        &mut self,
        handle: &client::Handle<H>,
        command: Command,
    ) -> Option<Command> {
        match command {
            Command::Forward(inner) => {
                self.on_app(handle, inner).await;
                None
            }
            Command::DirectTcp {
                host,
                port,
                origin_host,
                origin_port,
                reply,
            } => {
                let opened = tokio::time::timeout(
                    OPEN_TIMEOUT,
                    handle.channel_open_direct_tcpip(host, port, origin_host, origin_port),
                )
                .await;
                let result = match opened {
                    Ok(result) => result,
                    Err(_) => Err(russh::Error::ConnectionTimeout),
                };
                let _ = reply.send(result);
                None
            }
            other => Some(other),
        }
    }

    async fn on_app<H: client::Handler>(
        &mut self,
        handle: &client::Handle<H>,
        command: ForwardCommand,
    ) {
        match command {
            ForwardCommand::Stop { id } => self.stop(handle, id).await,
            ForwardCommand::Start(spec) => self.start(handle, spec).await,
        }
    }

    async fn start<H: client::Handler>(&mut self, handle: &client::Handle<H>, spec: ForwardSpec) {
        if self.jobs.contains_key(&spec.id) {
            self.stop(handle, spec.id).await;
        }
        let started = match spec.kind {
            ForwardKind::Local => self.start_local(handle, spec.clone()).await,
            ForwardKind::Dynamic => self.start_dynamic(handle, spec.clone()).await,
            ForwardKind::Remote => self.start_remote(handle, spec.clone()).await,
        };
        if let Err(failure) = started {
            let _ = self.events.send(ForwardEvent::Failed {
                id: spec.id,
                generation: spec.generation,
                failure,
            });
        }
    }

    async fn start_local<H: client::Handler>(
        &mut self,
        handle: &client::Handle<H>,
        spec: ForwardSpec,
    ) -> Result<(), ForwardFailure> {
        let target_host = spec
            .target_host
            .clone()
            .ok_or_else(|| ForwardFailure::Other {
                detail: "a local forward needs a target. The rule was not started.".to_owned(),
            })?;
        let target_port = spec.target_port.ok_or_else(|| ForwardFailure::Other {
            detail: "a local forward needs a target port. The rule was not started.".to_owned(),
        })?;
        let listener = bind_listener(&spec.listen_host, spec.listen_port).await?;
        // The probe uses the connection handle directly. Sending it back through the
        // command channel would wait for this same loop and never return.
        if let Err(failure) = probe_handle(handle, &target_host, target_port).await {
            drop(listener);
            return Err(failure);
        }
        self.spawn_local(
            spec,
            listener,
            Some((target_host, target_port)),
            None,
            false,
        );
        Ok(())
    }

    async fn start_dynamic<H: client::Handler>(
        &mut self,
        handle: &client::Handle<H>,
        spec: ForwardSpec,
    ) -> Result<(), ForwardFailure> {
        let listener = bind_listener(&spec.listen_host, spec.listen_port).await?;
        // A closed port proves the server allows direct-tcpip. An administrative
        // refusal means this start did not listen.
        match probe_handle(handle, "127.0.0.1", 1).await {
            Ok(_) => {}
            Err(ForwardFailure::TargetUnreachable { .. }) => {}
            Err(failure) => {
                drop(listener);
                return Err(failure);
            }
        }
        self.spawn_local(spec, listener, None, None, true);
        Ok(())
    }

    async fn start_remote<H: client::Handler>(
        &mut self,
        handle: &client::Handle<H>,
        spec: ForwardSpec,
    ) -> Result<(), ForwardFailure> {
        let target_host = spec
            .target_host
            .clone()
            .ok_or_else(|| ForwardFailure::Other {
                detail: "a remote forward needs a target. The rule was not started.".to_owned(),
            })?;
        let target_port = spec.target_port.ok_or_else(|| ForwardFailure::Other {
            detail: "a remote forward needs a target port. The rule was not started.".to_owned(),
        })?;
        match tokio::time::timeout(
            OPEN_TIMEOUT,
            TcpStream::connect(socket_text(&target_host, target_port)),
        )
        .await
        {
            Ok(Ok(stream)) => drop(stream),
            Ok(Err(error)) => {
                return Err(ForwardFailure::TargetUnreachable {
                    host: target_host,
                    port: target_port,
                    detail: error.to_string(),
                });
            }
            Err(_) => {
                return Err(ForwardFailure::TargetUnreachable {
                    host: target_host,
                    port: target_port,
                    detail: "timed out".to_owned(),
                });
            }
        }
        let granted = tokio::time::timeout(
            OPEN_TIMEOUT,
            handle.tcpip_forward(spec.listen_host.clone(), u32::from(spec.listen_port)),
        )
        .await
        .map_err(|_| ForwardFailure::ServerRefused {
            detail: "timed out waiting for the server to listen. The rule was not started."
                .to_owned(),
        })?;
        let port = match granted {
            Ok(0) => spec.listen_port,
            Ok(port) => u16::try_from(port).unwrap_or(spec.listen_port),
            Err(error) => {
                return Err(ForwardFailure::ServerRefused {
                    detail: error.to_string(),
                });
            }
        };
        self.gate.install(InboundRule {
            id: spec.id,
            generation: spec.generation,
            listen_host: spec.listen_host.clone(),
            listen_port: port,
            target_host,
            target_port,
            alive: Arc::new(AtomicBool::new(true)),
        });
        let (stop, _stop_rx) = watch::channel(false);
        self.jobs.insert(
            spec.id,
            Job {
                generation: spec.generation,
                remote: Some((spec.listen_host.clone(), port)),
                stop,
                task: tokio::spawn(std::future::pending()),
                bridges: Arc::new(Mutex::new(Vec::new())),
            },
        );
        let _ = self.events.send(ForwardEvent::Listening {
            id: spec.id,
            generation: spec.generation,
            host: spec.listen_host,
            port,
        });
        Ok(())
    }

    fn spawn_local(
        &mut self,
        spec: ForwardSpec,
        listener: TcpListener,
        target: Option<(String, u16)>,
        remote: Option<(String, u16)>,
        dynamic: bool,
    ) {
        let (stop, stop_rx) = watch::channel(false);
        let bridges = Arc::new(Mutex::new(Vec::new()));
        let open = self.open.clone();
        let events = self.events.clone();
        let id = spec.id;
        let generation = spec.generation;
        let bound = listener.local_addr().ok();
        let bound_port = bound.map(|addr| addr.port()).unwrap_or(spec.listen_port);
        let bound_host = spec.listen_host.clone();
        let task = tokio::spawn(accept_loop(
            listener,
            stop_rx,
            open,
            events,
            bridges.clone(),
            id,
            generation,
            target,
            dynamic,
        ));
        self.jobs.insert(
            id,
            Job {
                generation,
                remote,
                stop,
                task,
                bridges,
            },
        );
        let _ = self.events.send(ForwardEvent::Listening {
            id,
            generation,
            host: bound_host,
            port: bound_port,
        });
    }

    async fn stop<H: client::Handler>(&mut self, handle: &client::Handle<H>, id: ForwardId) {
        let Some(job) = self.jobs.remove(&id) else {
            return;
        };
        let generation = job.generation;
        let remote = job.remote.clone();
        job.shutdown();
        for task in self.gate.take_tasks(Some(id)) {
            task.abort();
        }
        if let Some((address, port)) = remote {
            let _ = tokio::time::timeout(
                OPEN_TIMEOUT,
                handle.cancel_tcpip_forward(address, u32::from(port)),
            )
            .await;
        }
        let _ = self.events.send(ForwardEvent::Stopped { id, generation });
    }

    /// Stops every listener before the connection is torn down.
    pub(crate) async fn shutdown<H: client::Handler>(&mut self, handle: &client::Handle<H>) {
        let jobs = std::mem::take(&mut self.jobs);
        for (id, job) in jobs {
            let generation = job.generation;
            let remote = job.remote.clone();
            job.shutdown();
            if let Some((address, port)) = remote {
                let _ = tokio::time::timeout(
                    OPEN_TIMEOUT,
                    handle.cancel_tcpip_forward(address, u32::from(port)),
                )
                .await;
            }
            let _ = self.events.send(ForwardEvent::Failed {
                id,
                generation,
                failure: ForwardFailure::CarrierLost,
            });
        }
        for task in self.gate.take_tasks(None) {
            task.abort();
        }
        let _ = self.events.send(ForwardEvent::CarrierLost);
    }
}

/// Accepts or rejects one inbound forwarded channel.
///
/// A channel that does not name an active rule is rejected. Dropping the reply
/// would also reject it; the rejection here is explicit so the default accept in
/// russh is never what decides.
pub(crate) async fn open_inbound(
    gate: &ForwardGate,
    channel: Channel<client::Msg>,
    connected_address: &str,
    connected_port: u32,
    reply: ChannelOpenHandle,
) {
    let Some(rule) = gate.lookup(connected_address, connected_port) else {
        reply
            .reject(ChannelOpenFailure::AdministrativelyProhibited)
            .await;
        return;
    };
    if !rule.alive.load(Ordering::SeqCst) {
        reply
            .reject(ChannelOpenFailure::AdministrativelyProhibited)
            .await;
        return;
    }
    reply.accept().await;
    let events = gate.events.clone();
    let alive = Arc::clone(&rule.alive);
    let id = rule.id;
    let generation = rule.generation;
    let target_host = rule.target_host.clone();
    let target_port = rule.target_port;
    let task = tokio::spawn(async move {
        if !alive.load(Ordering::SeqCst) {
            return;
        }
        match TcpStream::connect(socket_text(&target_host, target_port)).await {
            Ok(stream) => relay(stream, channel).await,
            Err(error) => {
                let _ = events.send(ForwardEvent::AttemptFailed {
                    id,
                    generation,
                    failure: ForwardFailure::TargetUnreachable {
                        host: target_host,
                        port: target_port,
                        detail: error.to_string(),
                    },
                });
            }
        }
    });
    gate.track(id, task);
}

/// Picks the active remote rule an inbound channel belongs to.
///
/// An exact address wins over a wildcard on the same port. A port or address that
/// no active rule owns matches nothing, which is a rejection.
pub(crate) fn select_inbound<'a>(
    rules: &'a [InboundRule],
    connected_host: &str,
    connected_port: u32,
) -> Option<&'a InboundRule> {
    let Ok(port) = u16::try_from(connected_port) else {
        return None;
    };
    let mut wildcard = None;
    for rule in rules {
        if !rule.alive.load(Ordering::SeqCst) || rule.listen_port != port {
            continue;
        }
        if hosts_equal(&rule.listen_host, connected_host) {
            return Some(rule);
        }
        if is_wildcard(&rule.listen_host) && wildcard.is_none() {
            wildcard = Some(rule);
        }
    }
    wildcard
}

fn hosts_equal(rule_host: &str, connected_host: &str) -> bool {
    normalize_host(rule_host) == normalize_host(connected_host)
}

fn is_wildcard(host: &str) -> bool {
    matches!(normalize_host(host).as_str(), "0.0.0.0" | "::" | "*")
}

fn normalize_host(host: &str) -> String {
    let host = host
        .trim()
        .trim_matches(|character| character == '[' || character == ']')
        .to_ascii_lowercase();
    match host.as_str() {
        "localhost" => "127.0.0.1".to_owned(),
        other => other.to_owned(),
    }
}

async fn bind_listener(host: &str, port: u16) -> Result<TcpListener, ForwardFailure> {
    TcpListener::bind(socket_text(host, port))
        .await
        .map_err(|error| classify_bind(host, port, &error))
}

fn classify_bind(host: &str, port: u16, error: &std::io::Error) -> ForwardFailure {
    let host = host.to_owned();
    match error.kind() {
        std::io::ErrorKind::AddrInUse => ForwardFailure::PortInUse { host, port },
        std::io::ErrorKind::PermissionDenied => ForwardFailure::PermissionDenied { host, port },
        std::io::ErrorKind::InvalidInput | std::io::ErrorKind::AddrNotAvailable => {
            ForwardFailure::InvalidAddress {
                host,
                port,
                detail: error.to_string(),
            }
        }
        _ => {
            let text = error.to_string().to_ascii_lowercase();
            if text.contains("in use") || text.contains("already") {
                ForwardFailure::PortInUse { host, port }
            } else if text.contains("permission") || text.contains("access is denied") {
                ForwardFailure::PermissionDenied { host, port }
            } else {
                ForwardFailure::InvalidAddress {
                    host,
                    port,
                    detail: error.to_string(),
                }
            }
        }
    }
}

fn socket_text(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!(
            "[{}]:{port}",
            host.trim_matches(|character| character == '[' || character == ']')
        )
    } else {
        format!("{host}:{port}")
    }
}

async fn probe_handle<H: client::Handler>(
    handle: &client::Handle<H>,
    host: &str,
    port: u16,
) -> Result<(), ForwardFailure> {
    let opened = tokio::time::timeout(
        OPEN_TIMEOUT,
        handle.channel_open_direct_tcpip(host, u32::from(port), "127.0.0.1", 0),
    )
    .await;
    match opened {
        Ok(Ok(channel)) => {
            let _ = channel.eof().await;
            let _ = channel.close().await;
            Ok(())
        }
        Ok(Err(error)) => Err(classify_open(host, port, &error)),
        Err(_) => Err(ForwardFailure::TargetUnreachable {
            host: host.to_owned(),
            port,
            detail: "timed out".to_owned(),
        }),
    }
}

fn classify_open(host: &str, port: u16, error: &russh::Error) -> ForwardFailure {
    match error {
        russh::Error::ChannelOpenFailure(ChannelOpenFailure::AdministrativelyProhibited)
        | russh::Error::RequestDenied => ForwardFailure::ServerRefused {
            detail: error.to_string(),
        },
        russh::Error::ChannelOpenFailure(ChannelOpenFailure::ConnectFailed) => {
            ForwardFailure::TargetUnreachable {
                host: host.to_owned(),
                port,
                detail: error.to_string(),
            }
        }
        other => ForwardFailure::Other {
            detail: other.to_string(),
        },
    }
}

#[allow(clippy::too_many_arguments)]
async fn accept_loop(
    listener: TcpListener,
    mut stop: watch::Receiver<bool>,
    open: UnboundedSender<Command>,
    events: UnboundedSender<ForwardEvent>,
    bridges: Arc<Mutex<Vec<JoinHandle<()>>>>,
    id: ForwardId,
    generation: u64,
    target: Option<(String, u16)>,
    dynamic: bool,
) {
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => {
                let Ok((mut stream, peer)) = accepted else {
                    break;
                };
                let (target, socks_version) = if dynamic {
                    match socks_target(&mut stream).await {
                        Ok(target) => target,
                        Err(detail) => {
                            let _ = events.send(ForwardEvent::AttemptFailed {
                                id,
                                generation,
                                failure: ForwardFailure::Other { detail },
                            });
                            continue;
                        }
                    }
                } else if let Some(target) = target.clone() {
                    (target, 0)
                } else {
                    break;
                };
                let (reply_tx, reply_rx) = oneshot::channel();
                let origin = peer.ip().to_string();
                if open
                    .send(Command::DirectTcp {
                        host: target.0.clone(),
                        port: u32::from(target.1),
                        origin_host: origin,
                        origin_port: u32::from(peer.port()),
                        reply: reply_tx,
                    })
                    .is_err()
                {
                    break;
                }
                let opened = match reply_rx.await {
                    Ok(opened) => opened,
                    Err(_) => break,
                };
                match opened {
                    Ok(channel) => {
                        if dynamic {
                            let _ = socks_reply(&mut stream, socks_version, true).await;
                        }
                        let bridge = tokio::spawn(relay(stream, channel));
                        if let Ok(mut bridges) = bridges.lock() {
                            bridges.push(bridge);
                        }
                    }
                    Err(error) => {
                        if dynamic {
                            let _ = socks_reply(&mut stream, socks_version, false).await;
                        }
                        let _ = events.send(ForwardEvent::AttemptFailed {
                            id,
                            generation,
                            failure: classify_open(&target.0, target.1, &error),
                        });
                    }
                }
            }
        }
    }
}

async fn relay(mut local: TcpStream, channel: Channel<client::Msg>) {
    let mut remote = channel.into_stream();
    let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
    let _ = local.shutdown().await;
}

async fn socks_target(stream: &mut TcpStream) -> Result<((String, u16), u8), String> {
    let mut version = [0u8; 1];
    stream
        .read_exact(&mut version)
        .await
        .map_err(|error| format!("the SOCKS client closed before a request: {error}"))?;
    match version[0] {
        5 => socks5_target(stream).await.map(|target| (target, 5)),
        4 => socks4_target(stream).await.map(|target| (target, 4)),
        other => Err(format!(
            "SOCKS version {other} is not supported. Only SOCKS4 and SOCKS5 CONNECT are accepted."
        )),
    }
}

async fn socks5_target(stream: &mut TcpStream) -> Result<(String, u16), String> {
    let mut methods = [0u8; 1];
    stream
        .read_exact(&mut methods)
        .await
        .map_err(|error| error.to_string())?;
    let mut offered = vec![0u8; usize::from(methods[0])];
    stream
        .read_exact(&mut offered)
        .await
        .map_err(|error| error.to_string())?;
    if !offered.contains(&0) {
        let _ = stream.write_all(&[5, 0xff]).await;
        return Err("the SOCKS client did not offer no-authentication".to_owned());
    }
    stream
        .write_all(&[5, 0])
        .await
        .map_err(|error| error.to_string())?;
    let mut header = [0u8; 4];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|error| error.to_string())?;
    if header[0] != 5 {
        return Err("the SOCKS request is not version 5".to_owned());
    }
    if header[1] != 1 {
        return Err(
            "only SOCKS CONNECT is supported. BIND and UDP are refused.".to_owned(),
        );
    }
    let host = match header[3] {
        1 => {
            let mut bytes = [0u8; 4];
            stream
                .read_exact(&mut bytes)
                .await
                .map_err(|error| error.to_string())?;
            std::net::Ipv4Addr::from(bytes).to_string()
        }
        4 => {
            let mut bytes = [0u8; 16];
            stream
                .read_exact(&mut bytes)
                .await
                .map_err(|error| error.to_string())?;
            std::net::Ipv6Addr::from(bytes).to_string()
        }
        3 => {
            let mut length = [0u8; 1];
            stream
                .read_exact(&mut length)
                .await
                .map_err(|error| error.to_string())?;
            let mut name = vec![0u8; usize::from(length[0])];
            stream
                .read_exact(&mut name)
                .await
                .map_err(|error| error.to_string())?;
            String::from_utf8(name).map_err(|_| "the SOCKS domain is not UTF-8".to_owned())?
        }
        other => {
            return Err(format!("SOCKS address type {other} is not supported"));
        }
    };
    let mut port_bytes = [0u8; 2];
    stream
        .read_exact(&mut port_bytes)
        .await
        .map_err(|error| error.to_string())?;
    let port = u16::from_be_bytes(port_bytes);
    if port == 0 {
        return Err("the SOCKS target port is 0".to_owned());
    }
    Ok((host, port))
}

async fn socks4_target(stream: &mut TcpStream) -> Result<(String, u16), String> {
    let mut header = [0u8; 7];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|error| error.to_string())?;
    // The first byte of `header` is the command: we already consumed the version.
    if header[0] != 1 {
        return Err("only SOCKS4 CONNECT is supported".to_owned());
    }
    let port = u16::from_be_bytes([header[1], header[2]]);
    let ip = [header[3], header[4], header[5], header[6]];
    // User id, discarded.
    loop {
        let mut byte = [0u8; 1];
        stream
            .read_exact(&mut byte)
            .await
            .map_err(|error| error.to_string())?;
        if byte[0] == 0 {
            break;
        }
    }
    let host = if ip[0] == 0 && ip[1] == 0 && ip[2] == 0 && ip[3] != 0 {
        let mut name = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            stream
                .read_exact(&mut byte)
                .await
                .map_err(|error| error.to_string())?;
            if byte[0] == 0 {
                break;
            }
            name.push(byte[0]);
        }
        String::from_utf8(name).map_err(|_| "the SOCKS4a domain is not UTF-8".to_owned())?
    } else {
        std::net::Ipv4Addr::from(ip).to_string()
    };
    if port == 0 {
        return Err("the SOCKS target port is 0".to_owned());
    }
    Ok((host, port))
}

async fn socks_reply(stream: &mut TcpStream, version: u8, ok: bool) -> std::io::Result<()> {
    if version == 4 {
        let code = if ok { 90 } else { 91 };
        return stream.write_all(&[0, code, 0, 0, 0, 0, 0, 0]).await;
    }
    stream
        .write_all(&[5, if ok { 0 } else { 5 }, 0, 1, 0, 0, 0, 0, 0, 0])
        .await
}

/// Creates the window's link and the gate the connection thread installs rules on.
///
/// Commands travel on the session's own channel, so the loop that owns the SSH
/// handle is the loop that binds and cancels. A second channel would wait for a
/// reader that does not exist.
pub(crate) fn channel(
    commands: UnboundedSender<Command>,
) -> (ForwardLink, UnboundedSender<ForwardEvent>, Arc<ForwardGate>) {
    let (events, event_rx) = unbounded_channel();
    let gate = Arc::new(ForwardGate::new(events.clone()));
    (ForwardLink::new(commands, event_rx), events, gate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(host: &str, port: u16) -> InboundRule {
        InboundRule {
            id: ForwardId::new(),
            generation: 1,
            listen_host: host.to_owned(),
            listen_port: port,
            target_host: "127.0.0.1".to_owned(),
            target_port: 80,
            alive: Arc::new(AtomicBool::new(true)),
        }
    }

    #[test]
    fn an_inbound_channel_matches_only_its_active_rule() {
        let specific = rule("127.0.0.1", 9000);
        let wildcard = rule("0.0.0.0", 9001);
        let same_port = rule("0.0.0.0", 9000);
        let rules = [specific.clone(), wildcard.clone(), same_port.clone()];
        assert_eq!(
            select_inbound(&rules, "127.0.0.1", 9000).map(|rule| rule.id),
            Some(specific.id)
        );
        assert_eq!(
            select_inbound(&rules, "localhost", 9000).map(|rule| rule.id),
            Some(specific.id),
            "localhost and 127.0.0.1 are the same loopback endpoint"
        );
        assert!(
            select_inbound(&[specific.clone()], "10.1.1.5", 9000).is_none(),
            "a specific rule does not accept a different address on its port"
        );
        assert_eq!(
            select_inbound(&rules, "10.1.1.5", 9000).map(|rule| rule.id),
            Some(same_port.id),
            "a wildcard on that port accepts an address the specific rule does not"
        );
        assert_eq!(
            select_inbound(&rules, "10.1.1.5", 9001).map(|rule| rule.id),
            Some(wildcard.id)
        );
        assert!(select_inbound(&rules, "127.0.0.1", 1).is_none());
        specific.alive.store(false, Ordering::SeqCst);
        assert_eq!(
            select_inbound(&rules, "127.0.0.1", 9000).map(|rule| rule.id),
            Some(same_port.id),
            "a stopped rule does not accept a late channel; the wildcard still does"
        );
        same_port.alive.store(false, Ordering::SeqCst);
        assert!(
            select_inbound(&rules, "127.0.0.1", 9000).is_none(),
            "no active rule means the channel is rejected"
        );
    }

    #[test]
    fn bind_errors_keep_their_cause() {
        let in_use = classify_bind(
            "127.0.0.1",
            9,
            &std::io::Error::new(std::io::ErrorKind::AddrInUse, "in use"),
        );
        assert!(matches!(in_use, ForwardFailure::PortInUse { port: 9, .. }));
        let denied = classify_bind(
            "0.0.0.0",
            80,
            &std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        );
        assert!(matches!(
            denied,
            ForwardFailure::PermissionDenied { port: 80, .. }
        ));
        let text = denied.to_string();
        assert!(text.contains("permission denied"), "{text}");
        assert!(!text.contains("listening"));
    }
}
