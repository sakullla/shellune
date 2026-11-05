//! The SSH terminal against a real OpenSSH server (R3, R4, R6, R8).
//!
//! Every test here talks to /usr/sbin/sshd, not to a stand-in: a real TCP handshake,
//! the real protocol, real password and public key authentication, a real pseudo
//! terminal with a real shell on the other side. The server is disposable and is
//! started by scripts/ssh-test-server.sh, so the machine's own sshd, its host keys
//! and its authorized_keys files are never touched.
//!
//! How the server is provided differs by host, and the difference is printed rather
//! than hidden:
//!
//! * on Windows the tests start OpenSSH inside WSL (Debian) themselves;
//! * anywhere else, or when WSL cannot be started, a test prints why it cannot run
//!   and returns, so a run without a controllable server never claims SSH support
//!   was verified.
//!
//! scripts/ssh-integration.sh does the same thing from outside Cargo and keeps the
//! output as evidence.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use shellune_core::{AuthMethod, SecretString};
use shellune_ssh::testserver::{Harness, KEY_PASSPHRASE};
use shellune_ssh::{
    ConnectError, ConnectRequest, KnownHostKey, PtySize, RemoteEvent, RemoteSession,
};
use shellune_storage::{HostKey, HostTrust, TrustStore, TrustVerdict};

/// How long any single wait may take.
const PATIENCE: Duration = Duration::from_secs(45);

/// The server the whole file shares, started once.
fn shared() -> &'static Option<Harness> {
    static SERVER: OnceLock<Option<Harness>> = OnceLock::new();
    SERVER.get_or_init(|| {
        if !Harness::is_available() {
            return None;
        }
        Harness::start("shared", Harness::free_port()).ok()
    })
}

/// What to do when this host cannot run the server.
fn skip(what: &str) -> bool {
    if shared().is_some() {
        return false;
    }
    eprintln!(
        "SKIPPED {what}: this host cannot start a disposable OpenSSH server, so no SSH \
         behaviour is verified here and none is claimed. On Windows the tests start one \
         through WSL by themselves; elsewhere scripts/ssh-test-server.sh has to be run on \
         a Linux host the tests can reach (scripts/ssh-integration.sh shows the sequence)."
    );
    true
}

fn trusted(fingerprint: &str) -> KnownHostKey {
    KnownHostKey::Trusted {
        fingerprint: fingerprint.to_owned(),
    }
}

fn connect(server: &Harness, auth: AuthMethod, known: KnownHostKey) -> RemoteSession {
    RemoteSession::connect(ConnectRequest::new(
        "127.0.0.1",
        server.port(),
        server.user(),
        auth,
        PtySize::new(100, 30),
        known,
    ))
}

/// The password attempt every test starts from.
fn password(server: &Harness) -> AuthMethod {
    AuthMethod::password(server.password())
}

/// Waits until one event decides the outcome, returning that event.
fn wait_for_outcome(session: &mut RemoteSession) -> RemoteEvent {
    let deadline = Instant::now() + PATIENCE;
    loop {
        for event in session.drain() {
            match event {
                RemoteEvent::HostKeyPrompt(_) | RemoteEvent::Connected | RemoteEvent::Failed(_) => {
                    return event;
                }
                RemoteEvent::Data(_) | RemoteEvent::Exited { .. } => {}
            }
        }
        if Instant::now() > deadline {
            panic!("the connection attempt neither connected nor failed in time");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Waits until the output contains a phrase, returning everything received.
fn wait_for_output(session: &mut RemoteSession, needle: &str) -> String {
    let deadline = Instant::now() + PATIENCE;
    let mut output = Vec::new();
    loop {
        for event in session.drain() {
            match event {
                RemoteEvent::Data(bytes) => output.extend_from_slice(&bytes),
                RemoteEvent::Failed(error) => {
                    panic!("the session failed while waiting for output: {error}")
                }
                RemoteEvent::Exited { code } => {
                    panic!(
                        "the remote shell exited ({code:?}) before it printed the expected text; \
                         received {:?}",
                        String::from_utf8_lossy(&output)
                    )
                }
                _ => {}
            }
        }
        let text = String::from_utf8_lossy(&output).into_owned();
        if text.contains(needle) {
            return text;
        }
        if Instant::now() > deadline {
            panic!("the remote shell never printed {needle:?}; received {text:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A trust store of this test process's own.
fn scratch_trust(name: &str) -> TrustStore {
    let root = shared()
        .as_ref()
        .map(|server| server.dir().join("trust").join(name))
        .unwrap_or_else(|| std::env::temp_dir().join("shellune-ssh-trust").join(name));
    let _ = std::fs::remove_dir_all(&root);
    TrustStore::at(root)
}

#[test]
fn an_unknown_server_identity_is_shown_with_its_algorithm_and_fingerprint() {
    if skip("unknown host identity") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let expected = server.host_key_fingerprint();

    let mut session = connect(server, password(server), KnownHostKey::Unknown);
    let outcome = wait_for_outcome(&mut session);
    let RemoteEvent::HostKeyPrompt(prompt) = outcome else {
        panic!("an unknown identity must be asked about, got {outcome:?}");
    };
    assert_eq!(prompt.port, server.port());
    assert_eq!(
        prompt.algorithm(),
        "ssh-ed25519",
        "the dialog names the algorithm the server presented"
    );
    assert_eq!(
        prompt.fingerprint(),
        expected,
        "the dialog shows the fingerprint ssh-keygen prints for that host key"
    );

    // Refusing must end the attempt: no session may appear behind a rejected host.
    assert!(session.answer_host_key(false), "the refusal is delivered");
    let outcome = wait_for_outcome(&mut session);
    match outcome {
        RemoteEvent::Failed(ConnectError::HostKeyRejected { presented }) => {
            assert_eq!(presented.fingerprint, expected);
        }
        other => panic!("a refused identity must fail the attempt, got {other:?}"),
    }

    let store = scratch_trust("rejected");
    assert!(
        store.load().expect("first run").is_empty(),
        "nothing may be stored when the user refuses"
    );
    assert!(
        !store.path().exists(),
        "a refusal must not write a trust file"
    );
}

#[test]
fn accepting_the_identity_connects_stores_it_and_reconnects_without_a_question() {
    if skip("accepting a host identity") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let expected = server.host_key_fingerprint();
    let store = scratch_trust("accepted");
    let mut trust = store.load().expect("a first run is an empty library");

    // First contact: the identity is unknown, the user accepts it, and only then is
    // the record written.
    let presented = HostKey::new(
        "ssh-ed25519",
        expected.clone(),
        "AAAAC3NzaC1lZDI1NTE5AAAAIK6Lk1FqcnKXBppTRsW38vMVUrRnrLv6rQbxs6aDboAs",
    )
    .expect("a comparable identity");
    assert_eq!(
        trust.verdict("127.0.0.1", server.port(), &presented),
        TrustVerdict::Unknown
    );

    let mut session = connect(server, password(server), KnownHostKey::Unknown);
    let RemoteEvent::HostKeyPrompt(_prompt) = wait_for_outcome(&mut session) else {
        panic!("the first contact must ask about the identity");
    };
    assert!(session.answer_host_key(true), "the acceptance is delivered");
    let outcome = wait_for_outcome(&mut session);
    assert!(
        matches!(outcome, RemoteEvent::Connected),
        "an accepted identity must connect, got {outcome:?}"
    );

    trust
        .accept("127.0.0.1", server.port(), &presented, 1_700_000_000)
        .expect("the accepted identity is stored");
    store.save(&trust).expect("the trust file is written");
    assert_eq!(
        store
            .load()
            .expect("reload")
            .verdict("127.0.0.1", server.port(), &presented),
        TrustVerdict::Trusted,
        "the record survives a reload, so the next start trusts the host"
    );

    // A real remote shell: input, Chinese output, and a size the server really sees.
    session
        .write("echo shellune-你好-ssh\n".as_bytes())
        .expect("input reaches the remote shell");
    let output = wait_for_output(&mut session, "shellune-你好-ssh");

    session
        .resize(PtySize::new(120, 40))
        .expect("the resize reaches the connection");
    session
        .write(b"stty size\n")
        .expect("input reaches the remote shell");
    // stty reads the size the pseudo terminal really has, so this line only appears
    // if the window-change request reached the server's terminal.
    let after_resize = wait_for_output(&mut session, "40 120");
    assert!(
        after_resize.contains("40 120"),
        "the remote terminal reports the size the window asked for: {after_resize:?}"
    );
    session
        .write(b"echo after-resize-ok\n")
        .expect("input still reaches the shell after the resize");
    wait_for_output(&mut session, "after-resize-ok");
    assert!(
        !output.is_empty(),
        "the shell answered before the resize as well"
    );

    session.write(b"exit\n").expect("the shell can be ended");
    let deadline = Instant::now() + PATIENCE;
    loop {
        if session
            .drain()
            .iter()
            .any(|event| matches!(event, RemoteEvent::Exited { .. }))
        {
            break;
        }
        assert!(Instant::now() < deadline, "the remote shell never exited");
        std::thread::sleep(Duration::from_millis(20));
    }

    // Second contact: the identity is known, so no question may be asked.
    let mut again = connect(server, password(server), trusted(&expected));
    let outcome = wait_for_outcome(&mut again);
    assert!(
        matches!(outcome, RemoteEvent::Connected),
        "a trusted identity connects without asking, got {outcome:?}"
    );
    again.close();
    session.close();
}

#[test]
fn a_changed_server_identity_is_blocked_and_the_recorded_one_is_kept() {
    if skip("changed host identity") {
        return;
    }
    // A server of its own, because this test changes that server's identity.
    static OWN: OnceLock<Option<Mutex<Harness>>> = OnceLock::new();
    let server = OWN.get_or_init(|| {
        Harness::start("changed", Harness::free_port())
            .ok()
            .map(Mutex::new)
    });
    let Some(server) = server.as_ref() else {
        panic!("the second disposable server could not be started");
    };
    let mut server = server.lock().unwrap_or_else(|error| error.into_inner());

    let first = server.host_key_fingerprint();
    let mut trust = HostTrust::new();
    let accepted = HostKey::new("ssh-ed25519", first.clone(), "accepted-public-key")
        .expect("a comparable identity");
    trust
        .accept("127.0.0.1", server.port(), &accepted, 1_700_000_000)
        .expect("the identity is accepted first");

    // The server swaps its host key, which is what a rebuilt machine or a man in the
    // middle looks like from here.
    server.rotate().expect("the server restarts with a new key");
    let second = server.host_key_fingerprint();
    assert_ne!(first, second, "the harness really changed the host key");

    let verdict = trust.verdict("127.0.0.1", server.port(), &accepted);
    assert_eq!(
        verdict,
        TrustVerdict::Trusted,
        "the stored record still says the old identity is trusted"
    );

    let known = KnownHostKey::Changed {
        algorithm: "ssh-ed25519".to_owned(),
        fingerprint: first.clone(),
    };
    let mut session = connect(&server, password(&server), known);
    match wait_for_outcome(&mut session) {
        RemoteEvent::Failed(ConnectError::HostKeyChanged {
            stored, presented, ..
        }) => {
            assert_eq!(
                stored, first,
                "the failure names the identity that was trusted"
            );
            assert_eq!(
                presented.fingerprint, second,
                "and the identity the server presents now"
            );
        }
        other => panic!("a changed identity must end the attempt, got {other:?}"),
    }

    // The record cannot be replaced by accepting the new identity.
    let changed = HostKey::new("ssh-ed25519", second.clone(), "presented-public-key")
        .expect("a comparable identity");
    assert!(
        trust
            .accept("127.0.0.1", server.port(), &changed, 1_700_000_100)
            .is_err(),
        "a different identity must not overwrite the accepted one"
    );
    let stored = trust
        .entry("127.0.0.1", server.port())
        .expect("the old record is still there");
    assert_eq!(stored.fingerprint(), first, "the old fingerprint survives");
}

#[test]
fn every_supported_authentication_method_reaches_the_same_server() {
    if skip("authentication methods") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let fingerprint = server.host_key_fingerprint();

    // Password: the right one connects.
    let mut session = connect(server, password(server), trusted(&fingerprint));
    assert!(
        matches!(wait_for_outcome(&mut session), RemoteEvent::Connected),
        "the documented password must connect"
    );
    session.close();

    // Password: the wrong one fails, and the failure carries no secret.
    let mut wrong = connect(
        server,
        AuthMethod::password("not-the-password"),
        trusted(&fingerprint),
    );
    match wait_for_outcome(&mut wrong) {
        RemoteEvent::Failed(error) => {
            assert!(
                !error.to_string().contains("not-the-password"),
                "an error must never repeat the password: {error}"
            );
            assert!(
                error.is_retryable(),
                "a rejected password is retryable with another method"
            );
        }
        other => panic!("a wrong password must not connect, got {other:?}"),
    }

    // Retrying with another method, on the same host, right after the failure.
    let mut keyed = connect(
        server,
        AuthMethod::key_file(server.client_key().to_string_lossy().into_owned(), None),
        trusted(&fingerprint),
    );
    let outcome = wait_for_outcome(&mut keyed);
    assert!(
        matches!(outcome, RemoteEvent::Connected),
        "the key the server authorizes must connect, got {outcome:?}"
    );
    keyed.close();

    // The same key again, this time protected by a passphrase.
    let mut encrypted = connect(
        server,
        AuthMethod::key_file(
            server.client_key_encrypted().to_string_lossy().into_owned(),
            Some(SecretString::new(KEY_PASSPHRASE)),
        ),
        trusted(&fingerprint),
    );
    assert!(
        matches!(wait_for_outcome(&mut encrypted), RemoteEvent::Connected),
        "a passphrase protected key must decrypt and connect"
    );
    encrypted.close();

    // A wrong passphrase is a decryption problem, not a server rejection.
    let mut wrong_passphrase = connect(
        server,
        AuthMethod::key_file(
            server.client_key_encrypted().to_string_lossy().into_owned(),
            Some(SecretString::new("not-the-passphrase")),
        ),
        trusted(&fingerprint),
    );
    match wait_for_outcome(&mut wrong_passphrase) {
        RemoteEvent::Failed(error) => {
            assert!(
                matches!(error, ConnectError::KeyDecryptFailed { .. })
                    || matches!(error, ConnectError::KeyEncrypted { .. }),
                "a wrong passphrase must be named as a key problem, got {error}"
            );
            assert!(
                !error.to_string().contains("not-the-passphrase"),
                "an error must never repeat the passphrase: {error}"
            );
        }
        other => panic!("a wrong passphrase must not connect, got {other:?}"),
    }

    // The system agent: whatever this machine really has, the answer must be a real
    // one, and an unavailable agent must be named.
    let kind = shellune_ssh::agent::default_kind();
    let mut agent = connect(server, AuthMethod::agent(kind), trusted(&fingerprint));
    match wait_for_outcome(&mut agent) {
        RemoteEvent::Connected => {
            eprintln!(
                "measured: the {} authenticated this connection",
                kind.label()
            );
        }
        RemoteEvent::Failed(error) => {
            assert!(
                matches!(
                    error,
                    ConnectError::AgentUnavailable { .. } | ConnectError::AgentHasNoIdentity { .. }
                ),
                "an agent that cannot be used must say so, got {error}"
            );
            eprintln!("measured: {} is unavailable: {error}", kind.label());
        }
        other => panic!("the agent attempt must end in a real answer, got {other:?}"),
    }
}

#[test]
fn an_address_that_refuses_the_connection_shows_no_session() {
    if skip("refused connection") {
        return;
    }
    let server = shared().as_ref().expect("a server");
    let fingerprint = server.host_key_fingerprint();
    // Port 1 on the loopback interface is not a server; the attempt must say so and
    // must never report a live session.
    let mut session = RemoteSession::connect(ConnectRequest::new(
        "127.0.0.1",
        1,
        server.user(),
        password(server),
        PtySize::new(80, 24),
        trusted(&fingerprint),
    ));
    match wait_for_outcome(&mut session) {
        RemoteEvent::Failed(error) => {
            assert!(
                error.is_transport_failure(),
                "a refused connection is a transport failure: {error}"
            );
        }
        other => panic!("an unreachable port must not connect, got {other:?}"),
    }
}
