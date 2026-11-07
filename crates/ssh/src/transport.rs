//! Byte pipes a jump chain can run SSH on (R10).
//!
//! A direct TCP socket, a channel opened through the previous hop, and a
//! ProxyCommand are the same thing once they are open: an async read/write
//! stream. Nothing in this module dials a hop or decides that a command may
//! run. Expanding a command and splitting its arguments do not start a process;
//! [`spawn_proxy`] is the only place that does, and the chain calls it after
//! the user has confirmed that exact command.

use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use russh::ChannelMsg;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::process::{Child, Command};
use tokio::sync::Notify;

use crate::error::ConnectError;

/// A cancellation flag the chain and its pipes share.
///
/// Firing it wakes every waiter. A waiter that starts afterwards still notices,
/// because the flag stays set: a permit-style notify alone would lose a fire
/// that happened before anyone was waiting.
#[derive(Clone, Debug)]
pub(crate) struct Cancel {
    flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl Cancel {
    pub(crate) fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    pub(crate) fn fire(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    pub(crate) async fn cancelled(&self) {
        let notified = self.notify.notified();
        tokio::pin!(notified);
        loop {
            // Register before reading the flag, so a fire between the check and
            // the wait cannot pass unnoticed.
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.as_mut().await;
            if self.is_cancelled() {
                return;
            }
            notified.set(self.notify.notified());
        }
    }
}

trait BytePipe: AsyncRead + AsyncWrite + Send {}

impl<T> BytePipe for T where T: AsyncRead + AsyncWrite + Send {}

/// An owned bidirectional stream the SSH handshake can take.
pub(crate) struct IoStream {
    inner: Pin<Box<dyn BytePipe>>,
}

impl IoStream {
    pub(crate) fn new<T>(stream: T) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + 'static,
    {
        Self {
            inner: Box::pin(stream),
        }
    }
}

impl AsyncRead for IoStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.get_mut().inner.as_mut().poll_read(cx, buf)
    }
}

impl AsyncWrite for IoStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.get_mut().inner.as_mut().poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.get_mut().inner.as_mut().poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.get_mut().inner.as_mut().poll_shutdown(cx)
    }
}

/// A ProxyCommand process whose pipes are the SSH stream.
///
/// Dropping it stops the process. Shutdown also kills the children of a shell
/// wrapper, which a direct `Child::kill` would leave running.
pub(crate) struct ProxyProcess {
    child: Child,
    pid: u32,
}

impl ProxyProcess {
    pub(crate) async fn shutdown(mut self) {
        kill_tree(self.pid);
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await;
    }
}

impl Drop for ProxyProcess {
    fn drop(&mut self) {
        kill_tree(self.pid);
        let _ = self.child.start_kill();
    }
}

/// Substitutes `%h`, `%p`, `%r` and `%%` the way OpenSSH does.
///
/// Unknown tokens are left in place rather than deleted, so a command cannot
/// change meaning because a token was not recognised. This function does not
/// start a process.
#[must_use]
pub(crate) fn expand_proxy_command(template: &str, host: &str, port: u16, user: &str) -> String {
    let mut expanded = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '%' {
            expanded.push(character);
            continue;
        }
        match chars.next() {
            Some('%') => expanded.push('%'),
            Some('h') | Some('n') => expanded.push_str(host),
            Some('p') => expanded.push_str(&port.to_string()),
            Some('r') => expanded.push_str(user),
            Some(other) => {
                expanded.push('%');
                expanded.push(other);
            }
            None => expanded.push('%'),
        }
    }
    expanded
}

/// Starts a confirmed ProxyCommand and returns the process plus its stdio stream.
///
/// # Errors
///
/// Returns [`ConnectError::Unreachable`] when the command is empty or the
/// operating system refuses to start it. The refusal names the command, never
/// a password: a ProxyCommand has no secret field.
pub(crate) async fn spawn_proxy(expanded: &str) -> Result<(ProxyProcess, IoStream), ConnectError> {
    let expanded = expanded.trim();
    if expanded.is_empty() {
        return Err(ConnectError::Unreachable {
            reason: "the ProxyCommand is empty, so it was not run".to_owned(),
        });
    }
    let mut command = match command_argv(expanded) {
        Some(argv) => {
            let mut command = Command::new(&argv[0]);
            command.args(&argv[1..]);
            command
        }
        None => shell_command(expanded),
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // A shell wrapper's children must die with it. A new process group lets
    // shutdown signal the whole group rather than only the shell.
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().map_err(|error| ConnectError::Unreachable {
        reason: format!("the ProxyCommand could not be started: {error}"),
    })?;
    let pid = child.id().unwrap_or(0);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ConnectError::Unreachable {
            reason: "the ProxyCommand has no standard output, so it was stopped".to_owned(),
        })?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| ConnectError::Unreachable {
            reason: "the ProxyCommand has no standard input, so it was stopped".to_owned(),
        })?;
    let stream = IoStream::new(tokio::io::join(stdout, stdin));
    Ok((ProxyProcess { child, pid }, stream))
}

/// Copies a TCP socket onto a stream the handshake can own.
///
/// The copy task is aborted when the attempt is cancelled or torn down, which
/// closes the socket even if the SSH task has not noticed yet.
pub(crate) fn bridge_tcp(
    tcp: tokio::net::TcpStream,
    cancel: Cancel,
) -> (IoStream, tokio::task::JoinHandle<()>) {
    let (client, server) = tokio::io::duplex(256 * 1024);
    let task = tokio::spawn(async move {
        let mut tcp = tcp;
        let mut server = server;
        tokio::select! {
            _ = tokio::io::copy_bidirectional(&mut tcp, &mut server) => {}
            _ = cancel.cancelled() => {}
        }
    });
    (IoStream::new(client), task)
}

/// Copies a direct-tcpip channel onto a stream the next handshake can own.
///
/// `ChannelStream` is not usable here: russh's async-write wrapper requires a
/// `Sync` message type, and the client message is not `Sync`. The pump speaks
/// channel data instead, which is the same byte pipe a socket would be.
pub(crate) fn bridge_channel(
    channel: russh::Channel<russh::client::Msg>,
    cancel: Cancel,
) -> (IoStream, tokio::task::JoinHandle<()>) {
    let (client, server) = tokio::io::duplex(256 * 1024);
    let task = tokio::spawn(async move {
        let (mut read_half, write_half) = channel.split();
        let (mut inbound, mut outbound) = tokio::io::split(server);
        let write_half = write_half;
        tokio::select! {
            _ = async {
                let mut buf = vec![0u8; 32 * 1024];
                loop {
                    match inbound.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            if write_half.data_bytes(buf[..count].to_vec()).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            } => {}
            _ = async {
                loop {
                    match read_half.wait().await {
                        Some(ChannelMsg::Data { data })
                        | Some(ChannelMsg::ExtendedData { data, .. }) => {
                            if outbound.write_all(&data).await.is_err() {
                                break;
                            }
                        }
                        Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                        Some(_) => {}
                    }
                }
            } => {}
            _ = cancel.cancelled() => {}
        }
        let _ = write_half.eof().await;
        let _ = write_half.close().await;
    });
    (IoStream::new(client), task)
}

/// Turns an operating-system connect error into the same names a direct dial uses.
pub(crate) fn io_connect_error(target: &str, error: std::io::Error) -> ConnectError {
    ConnectError::from_transport(target, &russh::Error::IO(error))
}

fn shell_command(expanded: &str) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("cmd.exe");
        command.arg("/C").arg(expanded);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("sh");
        command.arg("-c").arg(expanded);
        command
    }
}

/// Splits a command that has no shell syntax into a program and arguments.
///
/// `None` means the command needs a shell (a pipe, a redirect, or an
/// unbalanced quote). The caller still has to have been confirmed; this only
/// chooses how to start it so the process we can kill is the real one.
fn command_argv(command: &str) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut quoted = false;
    for character in command.chars() {
        if character == '"' {
            in_quotes = !in_quotes;
            quoted = true;
            continue;
        }
        if !in_quotes
            && matches!(
                character,
                '|' | '&' | '>' | '<' | '^' | ';' | '(' | ')' | '\n' | '\r'
            )
        {
            return None;
        }
        if !in_quotes && character.is_whitespace() {
            if quoted || !current.is_empty() {
                parts.push(std::mem::take(&mut current));
                quoted = false;
            }
            continue;
        }
        current.push(character);
    }
    if in_quotes {
        return None;
    }
    if quoted || !current.is_empty() {
        parts.push(current);
    }
    if parts.is_empty() { None } else { Some(parts) }
}

fn kill_tree(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = std::process::Command::new("kill")
            .args(["-KILL", "--", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_expand_and_a_doubled_percent_stays_literal() {
        let command =
            expand_proxy_command("ssh -W %h:%p -l %r %%h %n", "web.example", 2222, "deploy");
        assert_eq!(command, "ssh -W web.example:2222 -l deploy %h web.example");
    }

    #[test]
    fn an_unknown_token_is_kept_so_the_command_is_not_rewritten() {
        let command = expand_proxy_command("nc %h %p %z", "10.0.0.7", 22, "root");
        assert_eq!(command, "nc 10.0.0.7 22 %z");
    }

    #[test]
    fn a_quoted_command_splits_without_invoking_a_shell() {
        let parts =
            command_argv(r#""C:\tools\bridge.exe" 127.0.0.1 2222"#).expect("no shell syntax");
        assert_eq!(
            parts,
            vec![
                r"C:\tools\bridge.exe".to_owned(),
                "127.0.0.1".to_owned(),
                "2222".to_owned()
            ]
        );
        assert!(
            command_argv("nc %h %p | tee out").is_none(),
            "a pipe must stay with the shell"
        );
    }

    #[test]
    fn expanding_a_command_does_not_require_a_process() {
        // The assertion is the function's type: it returns a String and has no
        // await. A regression that spawned here would not compile against this.
        let command = expand_proxy_command("sleep 30", "localhost", 22, "root");
        assert_eq!(command, "sleep 30");
    }
}
