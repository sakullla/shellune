//! Wire-level lifecycle regressions against a local SSH peer, without external sshd.
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use russh::{Channel, ChannelId, Disconnect, server};
use shellune_core::{AuthMethod, ConnectionDefaults, HostConfig};

use crate::{
    ChainEvent, ChainRequest, ChainSession, ConnectRequest, HopAuth, KnownHostKey, PtySize,
    RemoteEvent, RemoteSession,
};

struct Peer {
    ready: mpsc::Sender<(server::Handle, ChannelId)>,
    channels: Vec<Channel<server::Msg>>,
}

impl server::Handler for Peer {
    type Error = russh::Error;

    async fn auth_password(&mut self, _: &str, _: &str) -> Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        self.channels.push(channel);
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        self.ready.send((session.handle(), channel)).unwrap();
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if data == b"blackhole" {
            // Keep the TCP connection open but stop processing SSH messages, including
            // probes. This distinguishes a dead peer from a healthy idle shell.
            return std::future::pending().await;
        }
        session.data(channel, data.to_vec())
    }
}

enum Client {
    Direct(RemoteSession),
    Chain(ChainSession),
}

impl Client {
    fn drain(&mut self) -> Vec<RemoteEvent> {
        match self {
            Self::Direct(session) => session.drain(),
            Self::Chain(session) => session
                .drain()
                .into_iter()
                .filter_map(|event| match event {
                    ChainEvent::Connected { .. } => Some(RemoteEvent::Connected),
                    ChainEvent::Data(data) => Some(RemoteEvent::Data(data)),
                    ChainEvent::Exited { code } => Some(RemoteEvent::Exited { code }),
                    ChainEvent::Failed(failure) => Some(RemoteEvent::Failed(
                        failure.connect_error().expect("a hop failure").clone(),
                    )),
                    _ => None,
                })
                .collect(),
        }
    }

    fn write(&mut self, data: &[u8]) {
        match self {
            Self::Direct(session) => session.write(data).unwrap(),
            Self::Chain(session) => session.write(data).unwrap(),
        }
    }

    fn wait(&mut self, predicate: impl Fn(&RemoteEvent) -> bool) -> RemoteEvent {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            for event in self.drain() {
                if predicate(&event) {
                    return event;
                }
                assert!(
                    !matches!(event, RemoteEvent::Failed(_) | RemoteEvent::Exited { .. }),
                    "unexpected terminal event: {event:?}"
                );
            }
            assert!(Instant::now() < deadline, "SSH event did not arrive");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn each_client(test: impl Fn(&mut Client, &tokio::runtime::Runtime, server::Handle, ChannelId)) {
    for chain in [false, true] {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let key = ssh_key::PrivateKey::from(ssh_key::private::Ed25519Keypair::from_seed(&[7; 32]));
        let known = KnownHostKey::Trusted {
            fingerprint: key
                .public_key()
                .fingerprint(ssh_key::HashAlg::Sha256)
                .to_string(),
        };
        let config = Arc::new(server::Config {
            keys: vec![key],
            inactivity_timeout: Some(Duration::from_secs(3)),
            ..Default::default()
        });
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let (ready, receiver) = mpsc::channel();
        let task = runtime.spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let running = server::run_stream(
                config,
                socket,
                Peer {
                    ready,
                    channels: Vec::new(),
                },
            )
            .await
            .unwrap();
            let _ = running.await;
        });
        let policy = ConnectionDefaults::new(1, 2, 10).unwrap().policy();
        let mut client = if chain {
            let host = HostConfig::new("idle-peer", "127.0.0.1", port, "test").unwrap();
            let auth = HopAuth::new(host.id(), AuthMethod::password("test"), known);
            Client::Chain(ChainSession::connect(
                ChainRequest::new(
                    vec![host.clone()],
                    host.id(),
                    vec![auth],
                    PtySize::default(),
                )
                .with_connection_policy(policy),
            ))
        } else {
            Client::Direct(RemoteSession::connect(
                ConnectRequest::new(
                    "127.0.0.1",
                    port,
                    "test",
                    AuthMethod::password("test"),
                    PtySize::default(),
                    known,
                )
                .with_connection_policy(policy),
            ))
        };
        client.wait(|event| matches!(event, RemoteEvent::Connected));
        let (handle, channel) = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        test(&mut client, &runtime, handle, channel);
        drop(client);
        task.abort();
    }
}

#[test]
fn healthy_idle_sessions_survive_multiple_keepalive_intervals() {
    each_client(|client, _, _, _| {
        // Longer than both the peer's idle limit and the client's missed-probe limit.
        std::thread::sleep(Duration::from_secs(5));
        for event in client.drain() {
            assert!(
                !matches!(event, RemoteEvent::Failed(_) | RemoteEvent::Exited { .. }),
                "{event:?}"
            );
        }
        client.write(b"still alive");
        client.wait(|event| matches!(event, RemoteEvent::Data(data) if data == b"still alive"));
    });
}

#[test]
fn server_disconnect_keeps_the_reason() {
    each_client(|client, runtime, handle, _| {
        runtime
            .block_on(handle.disconnect(
                Disconnect::ByApplication,
                "idle policy expired".to_owned(),
                "en".to_owned(),
            ))
            .unwrap();
        let event = client.wait(|event| matches!(event, RemoteEvent::Failed(_)));
        let RemoteEvent::Failed(error) = event else {
            unreachable!()
        };
        assert!(error.to_string().contains("idle policy expired"), "{error}");
    });
}

#[test]
fn an_unresponsive_peer_reports_keepalive_timeout() {
    each_client(|client, _, _, _| {
        client.write(b"blackhole");
        let event = client.wait(|event| matches!(event, RemoteEvent::Failed(_)));
        let RemoteEvent::Failed(error) = event else {
            unreachable!()
        };
        assert!(
            error.to_string().to_lowercase().contains("keepalive"),
            "{error}"
        );
    });
}

#[test]
fn eof_does_not_close_the_session_before_exit_status() {
    each_client(|client, runtime, handle, channel| {
        runtime.block_on(handle.eof(channel)).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        for event in client.drain() {
            assert!(
                !matches!(event, RemoteEvent::Failed(_) | RemoteEvent::Exited { .. }),
                "EOF is only a half-close: {event:?}"
            );
        }
        runtime.block_on(async {
            handle.exit_status_request(channel, 42).await.unwrap();
            handle.close(channel).await.unwrap();
        });
        let event = client.wait(|event| matches!(event, RemoteEvent::Exited { .. }));
        assert!(
            matches!(event, RemoteEvent::Exited { code: Some(42) }),
            "{event:?}"
        );
    });
}
