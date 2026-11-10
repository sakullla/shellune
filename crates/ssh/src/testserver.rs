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
//!
//! The disposable server allows TCP forwarding, so a jump test can open a
//! direct-tcpip channel through it. Starting, stopping or rotating the server
//! never runs a ProxyCommand.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The passphrase the harness gives the second copy of the client key.
pub const KEY_PASSPHRASE: &str = "shellune-key-passphrase";

/// What a disposable server does with an `x11-req` (R11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum X11Server {
    /// Forwarding allowed, with the system `xauth`.
    Enabled,
    /// Forwarding turned off, as a server that was configured that way.
    Disabled,
    /// Forwarding allowed but no `xauth` to prepare the remote display with.
    WithoutXauth,
}

impl X11Server {
    /// The lines that decide the behaviour, appended to the configuration.
    fn config_lines(self) -> String {
        match self {
            Self::Enabled => "X11Forwarding yes\nXAuthLocation /usr/bin/xauth\n".to_owned(),
            Self::Disabled => "X11Forwarding no\nXAuthLocation /usr/bin/xauth\n".to_owned(),
            // A path that does not exist is how a server without xauth behaves: the
            // request is refused because the display cannot be prepared, not because
            // forwarding is off, which is a different reason to show the user.
            Self::WithoutXauth => {
                "X11Forwarding yes\nXAuthLocation /nonexistent/shellune-xauth\n".to_owned()
            }
        }
    }
}

/// A headless X server inside the same environment, for the display tests (R11).
///
/// It is Xvfb with a cookie-enforcing authority file, so a program that reaches it has
/// really presented the right cookie: a bridge that dropped or reused the wrong one
/// would be refused by the server instead of quietly passing.
///
/// Nothing here is installed automatically. A machine without Xvfb reports that it
/// cannot run the test rather than pretending the test passed.
pub struct DisplayServer {
    number: u32,
    dir: PathBuf,
    running: bool,
}

impl DisplayServer {
    /// Starts Xvfb on a display nothing else is using.
    ///
    /// # Errors
    ///
    /// Returns the reason the display could not be started, which includes "this host
    /// has no Xvfb".
    pub fn start(name: &str) -> Result<Self, String> {
        let dir = harness_root().join(name).join("x11");
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        std::fs::write(dir.join("x11-display.sh"), DISPLAY_SCRIPT)
            .map_err(|error| error.to_string())?;
        let mut last = String::new();
        for number in 96..=99u32 {
            if display_taken(number) {
                continue;
            }
            let authority = dir.join(format!("Xauthority.{number}"));
            std::fs::write(&authority, wildcard_authority(&random_cookie()))
                .map_err(|error| error.to_string())?;
            let mut display = Self {
                number,
                dir: dir.clone(),
                running: false,
            };
            match display.launch() {
                Ok(()) => return Ok(display),
                Err(reason) => last = reason,
            }
        }
        Err(if last.is_empty() {
            "every display number the harness uses is already taken".to_owned()
        } else {
            last
        })
    }

    /// Whether this machine can run a headless X server at all.
    pub fn is_available() -> bool {
        Harness::is_available()
            && Command::new("wsl.exe")
                .args([
                    "-d",
                    "Debian",
                    "-u",
                    "root",
                    "--",
                    "test",
                    "-x",
                    "/usr/bin/Xvfb",
                ])
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
    }

    /// The display as this machine names it, which is what the product reads.
    #[must_use]
    pub fn display(&self) -> String {
        format!("127.0.0.1:{}", self.number)
    }

    /// The display number, for the `:N` form and for a socket name.
    #[must_use]
    pub fn number(&self) -> u32 {
        self.number
    }

    /// The authority file holding this display's cookie.
    #[must_use]
    pub fn authority(&self) -> PathBuf {
        self.dir.join(format!("Xauthority.{}", self.number))
    }

    /// The cookie of this display, hex encoded, as this machine's own file holds it.
    ///
    /// A forwarding test compares it with what the server was given: the real cookie
    /// must never be the one that travelled over SSH.
    #[must_use]
    pub fn cookie_hex(&self) -> String {
        let bytes = std::fs::read(self.authority()).unwrap_or_default();
        // One wildcard record: family, address length, number length, name length and
        // the name itself, then the length and the bytes of the cookie.
        let start = 2 + 2 + 2 + 2 + crate::x11::AUTH_NAME.len() + 2;
        let cookie = bytes.get(start..start + 16).unwrap_or_default();
        cookie.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// The TCP port the server listens on.
    #[must_use]
    pub fn port(&self) -> u16 {
        6000 + u16::try_from(self.number).unwrap_or(0)
    }

    /// The number of top-level windows the X server has.
    ///
    /// Used to show that a remote program really opened a window in this display and
    /// not only that its connection was accepted.
    ///
    /// # Errors
    ///
    /// Returns the reason the server could not be asked.
    pub fn window_count(&self) -> Result<usize, String> {
        let out = self.run("windows")?;
        Ok(out.trim().parse::<usize>().unwrap_or(0))
    }

    /// How many clients are connected to the X server right now.
    ///
    /// The display tests read it after a session was closed: a bridge that outlived
    /// the session would keep a connection here.
    ///
    /// # Errors
    ///
    /// Returns the reason the server could not be asked.
    pub fn connections(&self) -> Result<usize, String> {
        let out = self.run("connections")?;
        Ok(out.trim().parse::<usize>().unwrap_or(0))
    }

    /// Stops the server and leaves nothing listening.
    pub fn stop(&mut self) {
        if !self.running {
            return;
        }
        self.running = false;
        let _ = self.run("stop");
    }

    /// Runs one step of the harness script for this display.
    ///
    /// The script is a file on the Linux side rather than a `bash -c` argument: wsl.exe
    /// re-quotes what it is given, and a command with quotes of its own arrives at bash
    /// mangled. The same rule is why scripts/ssh-test-server.sh exists.
    fn run(&self, action: &str) -> Result<String, String> {
        let script = self.dir.join("x11-display.sh");
        let output = Command::new("wsl.exe")
            .args(["-d", "Debian", "-u", "root", "--", "bash"])
            .arg(wsl_path(&script))
            .arg(action)
            .arg(wsl_path(&self.dir))
            .arg(self.number.to_string())
            .arg(self.port().to_string())
            .output()
            .map_err(|error| format!("wsl.exe could not be run: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "the display server's {action} failed: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn launch(&mut self) -> Result<(), String> {
        self.run("start")?;
        // The script reports the server as listening inside the environment; this
        // side confirms the port is reachable from the machine the product runs on,
        // which is what the bridge will really use.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], self.port())),
                std::time::Duration::from_millis(200),
            )
            .is_ok()
            {
                self.running = true;
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let log_text = std::fs::read_to_string(self.dir.join(format!("xvfb-{}.log", self.number)))
            .unwrap_or_default();
        Err(format!(
            "Xvfb :{} is not reachable on port {}: {log_text}",
            self.number,
            self.port()
        ))
    }
}
/// The script that owns the headless display on the Linux side.
///
/// It is a file rather than a `bash -c` argument because wsl.exe re-quotes what it is
/// given: a command that carries quotes of its own reaches bash mangled, which is the
/// same reason scripts/ssh-test-server.sh exists.
const DISPLAY_SCRIPT: &str = r#"#!/usr/bin/env bash
# A disposable headless X server for Shellune's X11 forwarding tests (R11).
#
#   x11-display.sh start <work-dir> <display-number> <port>
#   x11-display.sh stop  <work-dir> <display-number> <port>
#   x11-display.sh windows     <work-dir> <display-number> <port>
#   x11-display.sh connections <work-dir> <display-number> <port>
#
# The server is started with an authority file, so a program that reaches it has really
# presented the right cookie; the forwarding tests depend on that. Everything it owns
# lives in the work directory and nothing outside it is touched.
set -uo pipefail

ACTION="${1:-start}"
WORK="${2:?the work directory is required}"
NUMBER="${3:-96}"
PORT="${4:-6096}"
AUTH="$WORK/Xauthority.$NUMBER"
LOG="$WORK/xvfb-$NUMBER.log"
PIDFILE="$WORK/xvfb-$NUMBER.pid"

case "$ACTION" in
start)
    # An interrupted earlier run can leave its own Xvfb behind; starting over it would
    # fail with "server already active" for a display nobody is using. The pattern is
    # anchored so this script's own command line cannot match it.
    pkill -f "^Xvfb :$NUMBER " 2>/dev/null || true
    chmod 1777 /tmp/.X11-unix 2>/dev/null || true
    rm -f "$LOG" "$PIDFILE"
    setsid Xvfb ":$NUMBER" -screen 0 800x600x24 -listen tcp -auth "$AUTH" \
        </dev/null >"$LOG" 2>&1 &
    echo $! >"$PIDFILE"
    # The launcher stays alive until the server listens: a session that ends in the
    # same breath as it started is torn down with the child it left behind.
    for _ in $(seq 1 100); do
        if ss -ltn 2>/dev/null | grep -q ":$PORT "; then
            echo "listening on :$NUMBER"
            exit 0
        fi
        sleep 0.1
    done
    echo "Xvfb :$NUMBER did not listen on $PORT" >&2
    cat "$LOG" >&2 || true
    exit 1
    ;;
stop)
    if [ -f "$PIDFILE" ]; then
        kill "$(cat "$PIDFILE")" 2>/dev/null || true
        rm -f "$PIDFILE"
    fi
    pkill -f "^Xvfb :$NUMBER " 2>/dev/null || true
    ;;
windows)
    # Top-level windows the display has, read with this display's own cookie.
    XAUTHORITY="$AUTH" DISPLAY=":$NUMBER" xwininfo -root -children 2>/dev/null \
        | grep -c '0x' || true
    ;;
connections)
    # Clients connected to this display right now.
    ss -Htn state established "sport = :$PORT" 2>/dev/null | wc -l
    ;;
*)
    echo "unknown action: $ACTION" >&2
    exit 2
    ;;
esac
"#;

impl Drop for DisplayServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Whether a display number is already served on this machine.
fn display_taken(number: u32) -> bool {
    let Ok(number) = u16::try_from(number) else {
        return true;
    };
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], 6000 + number)),
        std::time::Duration::from_millis(250),
    )
    .is_ok()
}

/// A random cookie for the headless display.
fn random_cookie() -> [u8; 16] {
    uuid::Uuid::new_v4().into_bytes()
}

/// An authority file whose single entry matches any client address.
///
/// A wildcard entry is what lets the test process - running on the Windows side and
/// reaching the server over TCP through WSL's forwarding - authenticate with the one
/// cookie the file holds.
fn wildcard_authority(cookie: &[u8; 16]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&65535u16.to_be_bytes());
    for field in [&b""[..], &b""[..], crate::x11::AUTH_NAME.as_bytes()] {
        out.extend_from_slice(&u16::try_from(field.len()).unwrap_or(0).to_be_bytes());
        out.extend_from_slice(field);
    }
    out.extend_from_slice(&16u16.to_be_bytes());
    out.extend_from_slice(cookie);
    out
}

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
    ///
    /// Starts a server that refuses TCP forwarding.
    ///
    /// The account and host key are the ordinary ones. Only `AllowTcpForwarding`
    /// changes, so a refusal is the server's and not a missing user.
    ///
    /// # Errors
    ///
    /// Returns the reason the server could not be started.
    pub fn start_without_forwarding(name: &str, port: u16) -> Result<Self, String> {
        let mut harness = Self::start(name, Self::free_port())?;
        harness.run("stop")?;
        harness.write_config(port, false, X11Server::Enabled)?;
        harness.port = port;
        Ok(harness)
    }

    pub fn start_without_sftp(name: &str, port: u16) -> Result<Self, String> {
        // The ordinary start creates the account, the keys and the authorized_keys
        // entry; this server reuses them and only changes what sshd is configured to
        // offer.
        let mut harness = Self::start(name, Self::free_port())?;
        harness.run("stop")?;
        harness.write_config(port, true, X11Server::Disabled)?;
        harness.port = port;
        Ok(harness)
    }

    /// Starts a server with a chosen X11 forwarding behaviour (R11).
    ///
    /// The account and host key are the ordinary ones, so a refusal is the server's
    /// configuration and not a broken fixture.
    ///
    /// # Errors
    ///
    /// Returns the reason the server could not be started.
    pub fn start_with_x11(name: &str, port: u16, x11: X11Server) -> Result<Self, String> {
        let mut harness = Self::start(name, Self::free_port())?;
        harness.run("stop")?;
        harness.write_config(port, true, x11)?;
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

    /// Runs one shell script as root inside the disposable server's environment.
    ///
    /// A few conditions can only be made from outside the account - a disk that is
    /// really full, for instance - and they have to be injected on the machine that
    /// runs the server instead of being faked in the client.
    ///
    /// # Errors
    ///
    /// Returns what the script wrote to the error stream.
    pub fn as_root(&self, script: &str) -> Result<String, String> {
        self.wsl(&["bash", "-c", script])
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

    /// Writes a configuration and starts sshd with it.
    ///
    /// The configuration is written from here rather than by a shell script on the
    /// Linux side: a heredoc would have to travel as one command-line argument, and
    /// wsl.exe expands command substitutions inside those before bash ever sees them.
    /// `allow` is `AllowTcpForwarding`, and `x11` decides what the server does with an
    /// `x11-req`.
    fn write_config(&self, port: u16, allow: bool, x11: X11Server) -> Result<(), String> {
        let allow = if allow { "yes" } else { "no" };
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
             AllowTcpForwarding {allow}\n\
             PermitOpen any\n\
             PermitListen any\n\
             MaxSessions 50\n\
             MaxStartups 50:30:200\n\
             PerSourcePenalties no\n\
             LogLevel DEBUG\n\
             {}",
            self.user,
            x11.config_lines()
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
