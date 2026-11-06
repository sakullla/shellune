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

    /// Starts a server that has the same account and host key but no SFTP subsystem.
    ///
    /// OpenSSH only offers SFTP when its configuration says so, which is what makes
    /// this the honest way to test "this server has no SFTP": the product talks to a
    /// real sshd that really refuses the subsystem, instead of to a stand-in that
    /// says so.
    ///
    /// # Errors
    ///
    /// Returns the reason the server could not be started, which includes "this host
    /// has no WSL to run a Linux server in".
    pub fn start_without_sftp(name: &str, port: u16) -> Result<Self, String> {
        // The ordinary start creates the account, the keys and the authorized_keys
        // entry; this server reuses them and only changes what sshd is configured to
        // offer.
        let mut harness = Self::start(name, Self::free_port())?;
        harness.run("stop")?;
        harness.write_config_without_sftp(port)?;
        harness.port = port;
        Ok(harness)
    }

    /// The home directory of the server's account.
    #[must_use]
    pub fn home(&self) -> String {
        format!("/home/{}", self.user)
    }

    /// Runs a shell script as the server's account.
    ///
    /// The fixtures and the checks use it: a directory whose name has Chinese
    /// characters and spaces, a directory the account cannot read, and the listing
    /// the server itself prints, which the browser's result is compared against.
    ///
    /// # Errors
    ///
    /// Returns what the script wrote to the error stream, so a failed fixture names
    /// itself instead of silently producing an empty directory.
    pub fn as_user(&self, script: &str) -> Result<String, String> {
        let file = self.dir.join("fixture.sh");
        let command = format!(
            "cat > '{file}' <<'SHELLUNE_EOF'
{script}
SHELLUNE_EOF
chmod 755 '{file}'
runuser -u '{user}' -- bash '{file}'",
            file = wsl_path(&file),
            user = self.user,
        );
        self.wsl(&["bash", "-c", &command])
    }

    /// Every line the server wrote to its log so far.
    ///
    /// The log is how a test shows that browsing a directory did not authenticate
    /// again: OpenSSH writes one line per accepted authentication, so the count
    /// before and after a listing is evidence about the connection that was used.
    ///
    /// # Errors
    ///
    /// Returns the reason the log could not be read.
    pub fn log(&self) -> Result<String, String> {
        let file = wsl_path(&self.dir.join("sshd.log"));
        self.wsl(&["bash", "-c", &format!("cat '{file}'")])
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

    /// Writes a configuration without an SFTP subsystem and starts a server with it.
    ///
    /// The configuration is written from here rather than by a shell script on the
    /// Linux side: a heredoc would have to travel as one command-line argument, and
    /// wsl.exe expands command substitutions inside those before bash ever sees them.
    fn write_config_without_sftp(&self, port: u16) -> Result<(), String> {
        let work = wsl_path(&self.dir);
        let config = format!(
            "Port {port}\n\
             ListenAddress 0.0.0.0\n\
             HostKey {work}/hostkey\n\
             PidFile {work}/sshd.pid\n\
             PasswordAuthentication yes\n\
             KbdInteractiveAuthentication no\n\
             PubkeyAuthentication yes\n\
             UsePAM no\n\
             PermitRootLogin no\n\
             AllowUsers {}\n\
             StrictModes no\n\
             PrintMotd no\n\
             PrintLastLog no\n\
             PerSourcePenalties no\n\
             LogLevel VERBOSE\n",
            self.user
        );
        std::fs::write(self.dir.join("sshd_config"), config).map_err(|error| error.to_string())?;
        let config = wsl_path(&self.dir.join("sshd_config"));
        let log = wsl_path(&self.dir.join("sshd.log"));
        self.wsl(&[
            "bash",
            "-c",
            &format!("/usr/sbin/sshd -t -f '{config}' && /usr/sbin/sshd -f '{config}' -E '{log}'"),
        ])?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                std::time::Duration::from_millis(200),
            )
            .is_ok()
            {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Err(format!("sshd did not start listening on {port}"))
    }

    /// Runs one command as root inside the disposable server's environment.
    fn wsl(&self, args: &[&str]) -> Result<String, String> {
        let output = Command::new("wsl.exe")
            .args(["-d", "Debian", "-u", "root", "--"])
            .args(args)
            .output()
            .map_err(|error| format!("wsl.exe could not be run: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "{}: {}{}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
