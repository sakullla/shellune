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

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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
            // Forwarding is allowed and there is no xauth anywhere for this server: the
            // program it is configured with does not exist, and the account finds none
            // either (see `hides_xauth`). The request is refused because the display
            // cannot be prepared, not because forwarding is off - a different reason to
            // show the user.
            Self::WithoutXauth => {
                "X11Forwarding yes\nXAuthLocation /nonexistent/shellune-xauth\n".to_owned()
            }
        }
    }

    /// Whether the account of this server has no `xauth` at all.
    ///
    /// A server whose account really has no `xauth`: the program is masked inside the
    /// server's own mount namespace, before sshd starts, and only there. The machine's
    /// `/usr/bin/xauth` is untouched, and so is every other test.
    fn hides_xauth(self) -> bool {
        matches!(self, Self::WithoutXauth)
    }
}

/// A headless X server inside the same environment, for the display tests (R11).
///
/// It is Xvfb with a cookie-enforcing authority file, so a program that reaches it has
/// really presented the right cookie: a bridge that dropped or reused the wrong one
/// would be refused by the server instead of quietly passing.
///
/// The one entry of that file is made by the real `xauth` (`xauth add :N …`), so its key
/// is the ordinary one a local display has: this machine's name and the display number,
/// FamilyLocal. That is also the key an entry issued by `xauth generate … untrusted` is
/// written under, which is what makes a mode that wrote into this file visible here.
///
/// Nothing here is installed automatically. A machine without Xvfb reports that it
/// cannot run the test rather than pretending the test passed.
pub struct DisplayServer {
    number: u32,
    dir: PathBuf,
    running: bool,
    /// Extra command-line words for Xvfb, such as taking an extension away.
    extra: String,
}

impl DisplayServer {
    /// Starts Xvfb on a display nothing else is using.
    ///
    /// # Errors
    ///
    /// Returns the reason the display could not be started, which includes "this host
    /// has no Xvfb".
    pub fn start(name: &str) -> Result<Self, String> {
        Self::start_with(name, &[])
    }

    /// Starts the same server without the X11 SECURITY extension.
    ///
    /// A display server that cannot issue an untrusted authorization is the case the
    /// restricted mode has to refuse rather than forward as trusted.
    ///
    /// # Errors
    ///
    /// Returns the reason the display could not be started.
    pub fn start_without_security(name: &str) -> Result<Self, String> {
        Self::start_with(name, &["-extension", "SECURITY"])
    }

    /// Starts Xvfb with extra command-line words.
    fn start_with(name: &str, extra: &[&str]) -> Result<Self, String> {
        let dir = harness_root().join(name).join("x11");
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        std::fs::write(dir.join("x11-display.sh"), DISPLAY_SCRIPT)
            .map_err(|error| error.to_string())?;
        let mut last = String::new();
        for number in 96..=99u32 {
            if display_taken(number) {
                continue;
            }
            let mut display = Self {
                number,
                dir: dir.clone(),
                running: false,
                extra: extra.join(" "),
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

    /// A directory to put in front of `PATH`, holding a stand-in `xauth`.
    ///
    /// The product looks for `xauth` on `PATH`, exactly as it does on a machine where
    /// the X tools are installed. This machine has no `xauth` of its own - the display
    /// lives inside the Linux environment the harness runs - so the stand-in passes the
    /// one command the restricted mode sends to that `xauth`. What it starts is the real
    /// /usr/bin/xauth of that environment, talking to the same display over the same
    /// TCP port, with the authority file reached through the shared drive. What it
    /// leaves behind is `last-issued`: the entry the display issued, copied out of the
    /// private file the product gave it before that file is removed.
    #[must_use]
    pub fn xauth_path(&self) -> PathBuf {
        let directory = self.dir.join("xauth-path");
        std::fs::create_dir_all(&directory).expect("a directory for the stand-in xauth");
        // The path the issued entry is copied to is written into the script: `%~dp0` is
        // the working directory of whoever started the batch file and not the directory
        // of the script, so it cannot be used to find a place next to the script.
        let capture = directory.join("last-issued");
        write_batch(
            &directory.join("xauth.cmd"),
            &XAUTH_SCRIPT.replace("@CAPTURE@", &capture.display().to_string()),
        );
        directory
    }

    /// A directory whose stand-in `xauth` always fails.
    ///
    /// This is what a machine looks like whose `xauth` cannot issue a restricted cookie:
    /// the program is there and says no, which is the case the product has to report
    /// instead of forwarding as trusted.
    #[must_use]
    pub fn broken_xauth_path(&self) -> PathBuf {
        let directory = self.dir.join("xauth-broken");
        std::fs::create_dir_all(&directory).expect("a directory for the failing xauth");
        write_batch(
            &directory.join("xauth.cmd"),
            "@echo off\necho xauth: the display cannot issue an untrusted cookie 1>&2\nexit /b 1\n",
        );
        directory
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

    /// The record of this display's authority file, as the product reads one.
    ///
    /// This is the entry this machine's own programs authenticate with. A restricted
    /// session must leave it exactly as it is; a session that wrote into this file would
    /// be caught here.
    #[must_use]
    pub fn authority_record(&self) -> AuthorityRecord {
        read_record(&std::fs::read(self.authority()).unwrap_or_default())
            .unwrap_or_else(|| panic!("{} holds no authority entry", self.authority().display()))
    }

    /// The entry the display issued for the last restricted session, if it issued one.
    ///
    /// The product removes the private file it read the entry from, so the stand-in
    /// `xauth` keeps a copy of what the display wrote. `None` means no restricted
    /// session asked this display for an untrusted cookie.
    #[must_use]
    pub fn issued_record(&self) -> Option<AuthorityRecord> {
        let copy = self.dir.join("xauth-path").join("last-issued");
        read_record(&std::fs::read(copy).ok()?)
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
        self.run_with(action, &self.extra)
    }

    /// The same, with the fifth argument the action needs.
    fn run_with(&self, action: &str, extra: &str) -> Result<String, String> {
        let script = self.dir.join("x11-display.sh");
        let output = Command::new("wsl.exe")
            .args(["-d", "Debian", "-u", "root", "--", "bash"])
            .arg(wsl_path(&script))
            .arg(action)
            .arg(wsl_path(&self.dir))
            .arg(self.number.to_string())
            .arg(self.port().to_string())
            .arg(extra)
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

    /// Whether this display server honours an authorization it issued as untrusted.
    ///
    /// A restricted session forwards with exactly such an authorization, so a server
    /// that refuses it refuses every remote program as well. The question is put to the
    /// server itself and not through the product: one untrusted authorization is issued
    /// into a private file of this test, its cookie is read back, and one raw
    /// `MIT-MAGIC-COOKIE-1` connection is opened with it.
    ///
    /// Debian's Xvfb 21.1.16 answers `xauth generate` with an authorization id and then
    /// refuses that cookie - only entries in the server's own `-auth` file are accepted
    /// there - and this reports exactly that instead of letting a test claim either way.
    #[must_use]
    pub fn honours_issued_cookies(&self) -> bool {
        let private = self.dir.join(format!("issued-{}.private", self.number));
        let _ = std::fs::remove_file(&private);
        if self.run_with("issue", &wsl_path(&private)).is_err() {
            return false;
        }
        let Some(cookie) = std::fs::read(&private)
            .ok()
            .and_then(|bytes| read_record(&bytes))
            .map(|record| hex_bytes(&record.cookie_hex))
        else {
            return false;
        };
        let address = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, self.port()));
        let Ok(mut stream) = std::net::TcpStream::connect_timeout(&address, Duration::from_secs(5))
        else {
            return false;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        if stream.write_all(&setup_request(&cookie)).is_err() {
            return false;
        }
        let mut answer = [0u8; 1];
        match stream.read_exact(&mut answer) {
            Ok(()) => answer[0] == 1,
            Err(_) => false,
        }
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
#   x11-display.sh start <work-dir> <display-number> <port> [extra Xvfb words]
#   x11-display.sh stop  <work-dir> <display-number> <port>
#   x11-display.sh windows     <work-dir> <display-number> <port>
#   x11-display.sh connections <work-dir> <display-number> <port>
#   x11-display.sh issue       <work-dir> <display-number> <port> <private file>
#
# The server is started with an authority file, so a program that reaches it has really
# presented the right cookie; the forwarding tests depend on that. Everything it owns
# lives in the work directory and nothing outside it is touched.
set -uo pipefail

ACTION="${1:-start}"
WORK="${2:?the work directory is required}"
NUMBER="${3:-96}"
PORT="${4:-6096}"
EXTRA="${5:-}"
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
    rm -f "$LOG" "$PIDFILE" "$AUTH"
    # The entry this machine's own programs authenticate with, made the way an ordinary
    # session makes it: `xauth add :NUMBER . cookie` normalises to the key `xauth` writes
    # for a local display (the machine name and the display number, FamilyLocal), which
    # is the same key a restricted session's untrusted entry would carry. Keeping that
    # shape is the point: a mode that wrote into this file would replace this entry, and
    # the test that reads it afterwards would see it.
    cookie="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
    if ! XAUTHORITY="$AUTH" xauth -q add ":$NUMBER" . "$cookie" 2>/dev/null; then
        echo "the authority file for :$NUMBER could not be written" >&2
        exit 1
    fi
    # shellcheck disable=SC2086 # the extra words are a command line of their own
    setsid Xvfb ":$NUMBER" -screen 0 800x600x24 -listen tcp -auth "$AUTH" $EXTRA \
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
issue)
    # One untrusted authorization for this display, written to the private file the
    # caller names. This is what the restricted mode asks for, done here without the
    # product so a test can find out whether this display server honours what it issues.
    PRIVATE="${5:?the file the entry goes to is required}"
    XAUTHORITY="$AUTH" xauth -f "$PRIVATE" generate "127.0.0.1:$NUMBER" \
        MIT-MAGIC-COOKIE-1 untrusted
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

/// The stand-in `xauth` the tests put on `PATH` (R11).
///
/// The restricted mode of the product looks for `xauth` the way a machine with the X
/// tools installed provides it. On this host the display belongs to the Linux
/// environment, so the stand-in hands the one command the product sends to that
/// environment's real `/usr/bin/xauth`, with both paths translated: the file `-f` names,
/// where the issued entry is written, and the authority file `XAUTHORITY` names, which
/// is the one the program authenticates to the display with. The two are different files
/// on purpose, and this stand-in keeps them apart: it only ever writes the first, and it
/// records what was written there so a test can read the entry the display issued even
/// though the product removes its private file again.
const XAUTH_SCRIPT: &str = r#"@echo off
rem Runs the real xauth of the disposable display for one invocation (R11).
setlocal enabledelayedexpansion
set "FILE="
set "REST="
:arguments
if "%~1"=="" goto translate
if /I "%~1"=="-f" goto take-file
set "REST=!REST! %1"
shift
goto arguments
:take-file
set "FILE=%~2"
shift
shift
goto arguments
:translate
for /f "usebackq delims=" %%p in (`wsl.exe -d Debian -u root -- wslpath -u "!FILE!"`) do set "ISSUED=%%p"
for /f "usebackq delims=" %%p in (`wsl.exe -d Debian -u root -- wslpath -u "%XAUTHORITY%"`) do set "TRUSTED=%%p"
rem The entry goes where -f says; the display is reached with the machine's own authority.
wsl.exe -d Debian -u root -- env XAUTHORITY="!TRUSTED!" /usr/bin/xauth -f "!ISSUED!"!REST!
set "STATUS=!ERRORLEVEL!"
rem The product removes its private file again, so the entry the display issued is copied
rem out for the test that reads it, to the path the harness wrote into this file.
if exist "!FILE!" copy /y "!FILE!" "@CAPTURE@" >nul 2>&1
exit /b !STATUS!
"#;

/// Writes a batch file the way `cmd.exe` expects it.
///
/// A `.cmd` file with bare line feeds is read by `cmd.exe` one byte stream at a time, and
/// a line that redirects output after a `for /f` loop is then not run at all - measured
/// here, and the reason this helper exists instead of a plain write.
fn write_batch(path: &Path, script: &str) {
    let text = script.replace("\r\n", "\n").replace('\n', "\r\n");
    std::fs::write(path, text).expect("the stand-in xauth is written");
}

/// Starts an OpenSSH server that has no usable `xauth` (R11).
///
/// The program is replaced by an empty, non-executable file bound over `/usr/bin/xauth`
/// inside a private mount namespace, and sshd is started in that namespace. The
/// machine's own `xauth` is untouched, and this server really has none: both sshd and
/// the account's own check fail for the reason a missing installation would.
const MASKED_SSH_SCRIPT: &str = r#"#!/usr/bin/env bash
#   sshd-without-xauth.sh <sshd config> <log file>
#
# The first run makes the mask and re-enters itself inside the namespace, where the mask
# is bound over the program. sshd daemonizes in there, so the mask lasts exactly as long
# as the server does.
set -uo pipefail
CONFIG="${1:?the sshd configuration is required}"
LOG="${2:?the log file is required}"

if [ "${SHELLUNE_XAUTH_MASKED:-}" != "1" ]; then
    mask="$(mktemp /tmp/shellune-xauth-mask.XXXXXX)"
    chmod 0644 "$mask"
    export SHELLUNE_XAUTH_MASKED=1
    export SHELLUNE_XAUTH_MASK="$mask"
    exec unshare -m --propagation private bash "$0" "$@"
fi

mount --bind "$SHELLUNE_XAUTH_MASK" /usr/bin/xauth
/usr/sbin/sshd -t -f "$CONFIG" && /usr/sbin/sshd -f "$CONFIG" -E "$LOG"
"#;

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

/// One record of an authority file, read the way the product reads one (R11).
///
/// A test compares the key (family, address and display number) and the cookie of the
/// entry this machine's own programs use with the entry a restricted session had the
/// display issue, and checks that the first one is still exactly what it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorityRecord {
    /// The address family: 256 is FamilyLocal, which a local display is named by.
    pub family: u16,
    /// The address of the family: the machine's own name for FamilyLocal.
    pub address: String,
    /// The display number, without the screen.
    pub number: String,
    /// The authentication protocol, `MIT-MAGIC-COOKIE-1` for this product.
    pub name: String,
    /// The cookie, hex encoded, so two of them cannot be confused by eye.
    pub cookie_hex: String,
}

impl AuthorityRecord {
    /// The key two records share when one would replace the other.
    ///
    /// `xauth` replaces an entry whose family, address, number and protocol all match,
    /// which is why a test compares these and not only the cookie.
    #[must_use]
    pub fn key(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.family, self.address, self.number, self.name
        )
    }
}

/// The first record of an authority file, in the shape the product reads.
fn read_record(bytes: &[u8]) -> Option<AuthorityRecord> {
    let entry = crate::x11::parse_authority(bytes).into_iter().next()?;
    Some(AuthorityRecord {
        family: entry.family,
        address: String::from_utf8_lossy(&entry.address).into_owned(),
        number: entry.number,
        name: entry.name,
        cookie_hex: entry
            .data
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    })
}

/// The bytes of a hex string, as an authority file holds them.
fn hex_bytes(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .filter_map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

/// One client's `MIT-MAGIC-COOKIE-1` setup request, the way the bridge sends it.
fn setup_request(cookie: &[u8]) -> Vec<u8> {
    let name = crate::x11::AUTH_NAME.as_bytes();
    let padding = |length: usize| (4 - length % 4) % 4;
    let mut request = vec![b'l', 0];
    request.extend_from_slice(&11u16.to_le_bytes());
    request.extend_from_slice(&0u16.to_le_bytes());
    request.extend_from_slice(&u16::try_from(name.len()).unwrap_or(0).to_le_bytes());
    request.extend_from_slice(&u16::try_from(cookie.len()).unwrap_or(0).to_le_bytes());
    request.extend_from_slice(&[0, 0]);
    request.extend_from_slice(name);
    request.extend(std::iter::repeat_n(0u8, padding(name.len())));
    request.extend_from_slice(cookie);
    request.extend(std::iter::repeat_n(0u8, padding(cookie.len())));
    request
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
        // A server without xauth is a server whose own `xauth` cannot run. The program
        // is masked by a bind mount inside the namespace sshd is started in, so the
        // machine keeps its xauth and this server really has none: the account's own
        // check and sshd's fail for the same reason a missing install would. The mask is
        // made by a script rather than a command line, because wsl.exe re-quotes what it
        // is given and a command carrying quotes of its own arrives mangled.
        let start = if x11.hides_xauth() {
            let script = self.dir.join("sshd-without-xauth.sh");
            std::fs::write(&script, MASKED_SSH_SCRIPT).map_err(|error| error.to_string())?;
            format!("bash '{}' '{config}' '{log}'", wsl_path(&script))
        } else {
            format!("/usr/sbin/sshd -t -f '{config}' && /usr/sbin/sshd -f '{config}' -E '{log}'")
        };
        self.wsl(&["bash", "-c", &start])?;
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
