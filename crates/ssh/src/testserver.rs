//! A disposable OpenSSH server for the tests that need a real one (R3, R4, R6, R8).
//!
//! The tests must talk to /usr/sbin/sshd, not to a stand-in: a real handshake, real
//! password and public key authentication, a real pseudo terminal with a real shell
//! behind it. This module starts exactly that, from scripts/ssh-test-server.sh, with
//! its own host key, its own user and its own authorized_keys, so the machine's own
//! sshd and files are never touched.
//!
//! How the server is provided differs by host, and the difference is printed rather
//! than hidden: on Windows the harness starts OpenSSH inside WSL, and anywhere else
//! it reports that it cannot run instead of pretending the tests passed.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The passphrase the harness gives the second copy of the client key.
pub const KEY_PASSPHRASE: &str = "shellune-key-passphrase";

/// A running disposable OpenSSH server.
#[derive(Clone, Debug)]
pub struct Harness {
    port: u16,
    dir: PathBuf,
    user: String,
    password: String,
    fingerprint: String,
}

impl Harness {
    /// Starts a server whose files live under the given name.
    ///
    /// The port is chosen by the caller, and a free one can be had from
    /// [Harness::free_port].
    ///
    /// # Errors
    ///
    /// Returns the reason the harness could not start, which includes "this host has
    /// no WSL to run a Linux server in".
    pub fn start(name: &str, port: u16) -> Result<Self, String> {
        let dir = harness_root().join(name);
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let mut harness = Self {
            port,
            dir,
            user: String::new(),
            password: String::new(),
            fingerprint: String::new(),
        };
        harness.run("start")?;
        if harness.user.is_empty() || harness.fingerprint.is_empty() {
            return Err("the harness did not report the server identity".to_owned());
        }
        Ok(harness)
    }

    /// Whether this machine can run a disposable server at all.
    pub fn is_available() -> bool {
        Command::new("wsl.exe")
            .args(["-d", "Debian", "-u", "root", "--", "true"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    /// A port nothing is listening on, asked for and then released.
    pub fn free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port can be reserved");
        listener.local_addr().expect("a local address").port()
    }

    /// Throws the host key away and starts again, so the identity changes.
    ///
    /// # Errors
    ///
    /// Returns the reason the server could not be restarted.
    pub fn rotate(&mut self) -> Result<(), String> {
        self.run("rotate")
    }

    /// Stops the server.
    pub fn stop(&mut self) {
        let _ = self.run("stop");
    }

    /// The TCP port the server listens on.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The account the server accepts.
    #[must_use]
    pub fn user(&self) -> &str {
        &self.user
    }

    /// That account's password, fixed so a test can try a wrong one and the right one.
    #[must_use]
    pub fn password(&self) -> &str {
        &self.password
    }

    /// The fingerprint OpenSSH prints for the server's host key.
    ///
    /// It comes from ssh-keygen, not from Shellune's own code, which is what makes
    /// the comparison an independent check of what the dialog shows.
    #[must_use]
    pub fn host_key_fingerprint(&self) -> String {
        self.fingerprint.clone()
    }

    /// The private key the server authorizes.
    #[must_use]
    pub fn client_key(&self) -> PathBuf {
        self.dir.join("client_key")
    }

    /// The same key, protected by [KEY_PASSPHRASE].
    #[must_use]
    pub fn client_key_encrypted(&self) -> PathBuf {
        self.dir.join("client_key_enc")
    }

    /// The directory the server's files live in.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Runs one harness action and adopts the identity block it reports.
    fn run(&mut self, action: &str) -> Result<(), String> {
        let script = repo_root().join("scripts").join("ssh-test-server.sh");
        let output = Command::new("wsl.exe")
            .args(["-d", "Debian", "-u", "root", "--", "bash"])
            .arg(wsl_path(&script))
            .arg(action)
            .arg(wsl_path(&self.dir))
            .arg(self.port.to_string())
            .output()
            .map_err(|error| format!("wsl.exe could not be run: {error}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.status.success() {
            return Err(format!(
                "{action} failed: {stdout}{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        for line in stdout.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "user" => self.user = value.trim().to_owned(),
                "password" => self.password = value.trim().to_owned(),
                "fingerprint" => self.fingerprint = value.trim().to_owned(),
                _ => {}
            }
        }
        Ok(())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The repository root, for the harness script.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// The directory the disposable servers and their keys live in.
fn harness_root() -> PathBuf {
    repo_root().join("target").join("ssh-harness")
}

/// Converts a Windows path into the path WSL sees.
fn wsl_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let bytes = text.as_bytes();
    if bytes.len() > 2 && bytes[1] == b':' {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        return format!("/mnt/{drive}{}", &text[2..]);
    }
    text
}
