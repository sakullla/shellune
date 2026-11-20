//! One RFB connection up to ServerInit: the version exchange, the security type,
//! authentication, TLS or VeNCrypt when the policy asks for encryption, and the
//! server's first description of its desktop.
use super::wire::read_exact;
use super::{Config, Ended};
use crate::control::{Control, Emit};
use crate::tls::{self, CERTIFICATE_REJECTED, SecureStream};
use crate::{Failure, MAX_FRAME_BYTES};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

/// How long the handshake may take before the attempt is abandoned.
const HANDSHAKE: Duration = Duration::from_secs(20);

/// The socket, and TLS once a security type asks for it.
///
/// One byte can be given back to the reader. VeNCrypt's TLS sub-types announce the
/// upgrade with a marker byte on some servers and start the handshake directly on
/// others, so the first byte after the sub-type is inspected and, when it is
/// already TLS, put back in front of the stream.
pub(super) struct Transport {
    kind: Kind,
    pushback: Option<u8>,
}

enum Kind {
    Plain(TcpStream),
    Secure(Box<SecureStream<Transport>>),
}

impl Transport {
    fn plain(socket: TcpStream) -> Transport {
        Transport {
            kind: Kind::Plain(socket),
            pushback: None,
        }
    }

    /// Puts one byte back so the next read returns it first.
    fn push_back(&mut self, byte: u8) {
        self.pushback = Some(byte);
    }

    /// Sets the read wait used by the update loop, through the TLS layer when one
    /// is in place.
    pub(super) fn set_read_timeout(&self, wait: Option<Duration>) {
        match &self.kind {
            Kind::Plain(stream) => {
                let _ = stream.set_read_timeout(wait);
            }
            Kind::Secure(stream) => stream.sock.set_read_timeout(wait),
        }
    }
}

impl Read for Transport {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if let Some(byte) = self.pushback.take() {
            buffer[0] = byte;
            return Ok(1);
        }
        match &mut self.kind {
            Kind::Plain(stream) => stream.read(buffer),
            Kind::Secure(stream) => stream.read(buffer),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match &mut self.kind {
            Kind::Plain(stream) => stream.write(buffer),
            Kind::Secure(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.kind {
            Kind::Plain(stream) => stream.flush(),
            Kind::Secure(stream) => stream.flush(),
        }
    }
}

/// What a completed handshake leaves behind.
pub(super) struct Opened {
    pub(super) transport: Transport,
    pub(super) width: u16,
    pub(super) height: u16,
}

/// Security types as RFB numbers them.
const NONE: u8 = 1;
const VNC_AUTH: u8 = 2;
const TLS: u8 = 18;
const VENCRYPT: u8 = 19;

/// Opens the connection and completes the handshake. A cancel at any step ends
/// the attempt with `Failure::Cancelled`, which the caller reports as Closed.
pub(super) fn open(config: &Config, emit: &Emit, control: &Arc<Control>) -> Result<Opened, Ended> {
    let socket = dial(config, control)?;
    let mut transport = Transport::plain(socket);
    let minor = version(&mut transport)?;
    super::trace(&format!("protocol version answered as 3.{minor}"));
    let chosen = security_type(&mut transport, config, minor)?;
    super::trace(&format!("security type {chosen}"));
    match chosen {
        NONE => {
            if minor >= 8 {
                result(&mut transport, minor)?;
            }
        }
        VNC_AUTH => {
            authenticate(&mut transport, &config.password)?;
            result(&mut transport, minor)?;
        }
        TLS => {
            if read_u8(&mut transport)? == 0 {
                return Err(Ended::new(Failure::Tls, "the server refused to start TLS"));
            }
            transport = secure(transport, config, emit, control)?;
            result(&mut transport, minor)?;
        }
        VENCRYPT => {
            transport = vencrypt(transport, config, emit, control)?;
        }
        other => {
            return Err(Ended::new(
                Failure::UnsupportedSecurity,
                format!(
                    "the server offered security type {other}, which this client cannot negotiate"
                ),
            ));
        }
    }
    if control.cancelled() {
        return Err(Ended::new(Failure::Cancelled, "cancelled"));
    }
    // ClientInit: every viewer may attach to the same session.
    send(&mut transport, &[1])?;
    let (width, height) = server_init(&mut transport)?;
    super::trace(&format!("desktop {width}x{height}"));
    let bytes = usize::from(width) * usize::from(height) * 4;
    if width == 0 || height == 0 || bytes > MAX_FRAME_BYTES {
        return Err(Ended::new(
            Failure::Limit,
            format!("a {width}x{height} desktop exceeds the {MAX_FRAME_BYTES} byte bound"),
        ));
    }
    Ok(Opened {
        transport,
        width,
        height,
    })
}

fn dial(config: &Config, control: &Arc<Control>) -> Result<TcpStream, Ended> {
    let address = format!("{}:{}", config.host, config.port);
    let endpoints = address
        .to_socket_addrs()
        .map_err(|error| Ended::io(error, Failure::Connect))?;
    let mut last = Ended::new(
        Failure::Connect,
        format!("no address resolved for the VNC server at {address}"),
    );
    let mut socket = None;
    for endpoint in endpoints {
        if control.cancelled() {
            return Err(Ended::new(Failure::Cancelled, "cancelled"));
        }
        match TcpStream::connect_timeout(&endpoint, HANDSHAKE) {
            Ok(stream) => {
                socket = Some(stream);
                break;
            }
            Err(error) => {
                last = Ended::new(
                    Failure::Connect,
                    format!("connect to {endpoint} failed: {error}"),
                )
            }
        }
    }
    let socket = socket.ok_or(last)?;
    socket.set_nodelay(true).ok();
    socket
        .set_read_timeout(Some(HANDSHAKE))
        .map_err(|error| Ended::io(error, Failure::Connect))?;
    control.attach(&socket);
    Ok(socket)
}

/// Reads the server's version and answers with one this client speaks. TigerVNC
/// announces 3.889 to mark its extensions; anything at or above 3.8 is answered
/// as 3.8.
fn version(transport: &mut Transport) -> Result<u8, Ended> {
    let mut banner = [0u8; 12];
    read_exact(transport, &mut banner).map_err(|error| Ended::io(error, Failure::Connect))?;
    let text = String::from_utf8_lossy(&banner);
    let (major, minor) = text
        .strip_prefix("RFB ")
        .and_then(|rest| rest.split_once('.'))
        .and_then(|(major, minor)| {
            Some((
                major.parse::<u16>().ok()?,
                minor.trim().parse::<u16>().ok()?,
            ))
        })
        .ok_or_else(|| {
            Ended::new(
                Failure::ProtocolFailure,
                format!("the server did not send an RFB banner ({})", text.trim()),
            )
        })?;
    if major != 3 {
        return Err(Ended::new(
            Failure::UnsupportedSecurity,
            format!("the server speaks RFB {major}.{minor}, which this client does not"),
        ));
    }
    let answer = if minor >= 8 {
        "RFB 003.008\n"
    } else if minor == 7 {
        "RFB 003.007\n"
    } else {
        "RFB 003.003\n"
    };
    send(transport, answer.as_bytes())?;
    Ok(if minor >= 8 {
        8
    } else {
        u8::try_from(minor).unwrap_or(8)
    })
}

/// Chooses the security type. The server's order decides which acceptable type
/// wins, so a server that prefers encryption gets it; a type outside the policy is
/// never attempted.
fn security_type(transport: &mut Transport, config: &Config, minor: u8) -> Result<u8, Ended> {
    if minor < 7 {
        let value = read_u32(transport)?;
        let chosen = u8::try_from(value).unwrap_or(0);
        let allowed = match chosen {
            NONE => config.security & 0x1 != 0,
            VNC_AUTH => config.security & 0x2 != 0,
            _ => false,
        };
        if !allowed {
            return Err(Ended::new(
                Failure::UnsupportedSecurity,
                format!("the server selected security type {value}, which this connection refuses"),
            ));
        }
        return Ok(chosen);
    }
    let count = read_u8(transport)?;
    if count == 0 {
        let reason = read_string(transport)?;
        return Err(Ended::new(
            Failure::UnsupportedSecurity,
            format!("the server offered no security types: {reason}"),
        ));
    }
    let mut types = vec![0u8; usize::from(count)];
    read_exact(transport, &mut types)?;
    let allowed = |kind: u8| match kind {
        NONE => config.security & 0x1 != 0,
        VNC_AUTH => config.security & 0x2 != 0,
        TLS => config.security & 0x4 != 0,
        VENCRYPT => config.security & 0x8 != 0,
        _ => false,
    };
    match types.iter().copied().find(|kind| allowed(*kind)) {
        Some(chosen) => {
            send(transport, &[chosen])?;
            Ok(chosen)
        }
        None => {
            // 0 names no type: the connection cannot go on, and saying so is the
            // protocol's own answer.
            let _ = send(transport, &[0]);
            Err(Ended::new(
                Failure::UnsupportedSecurity,
                format!(
                    "the server offers only security types this client refuses ({})",
                    types
                        .iter()
                        .map(u8::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            ))
        }
    }
}

/// VNC authentication: the server's 16-byte challenge encrypted with the
/// password as a DES key.
fn authenticate(transport: &mut Transport, password: &str) -> Result<(), Ended> {
    let mut challenge = [0u8; 16];
    read_exact(transport, &mut challenge)?;
    let answer = response(password, &challenge);
    send(transport, &answer)
}

fn response(password: &str, challenge: &[u8; 16]) -> [u8; 16] {
    use des::Des;
    use des::cipher::{BlockCipherEncrypt, KeyInit};
    let mut key = [0u8; 8];
    for (slot, byte) in key.iter_mut().zip(password.as_bytes()) {
        // The protocol reverses the bits of each password byte.
        *slot = byte.reverse_bits();
    }
    let cipher = Des::new_from_slice(&key).expect("a DES key is eight bytes");
    let mut out = [0u8; 16];
    for half in 0..2 {
        let start = half * 8;
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&challenge[start..start + 8]);
        let mut block: des::cipher::Block<Des> = bytes.into();
        cipher.encrypt_block(&mut block);
        out[start..start + 8].copy_from_slice(&block);
    }
    out
}

/// Reads the server's SecurityResult and turns a refusal into the authentication
/// failure it is.
fn result(transport: &mut Transport, minor: u8) -> Result<(), Ended> {
    let value = read_u32(transport)?;
    if value == 0 {
        return Ok(());
    }
    let reason = if minor >= 8 {
        read_string(transport).unwrap_or_default()
    } else {
        String::new()
    };
    Err(Ended::new(
        Failure::Authentication,
        if reason.is_empty() {
            format!("the server refused the credentials (result {value})")
        } else {
            format!("the server refused the credentials: {reason}")
        },
    ))
}

fn secure(
    transport: Transport,
    config: &Config,
    emit: &Emit,
    control: &Arc<Control>,
) -> Result<Transport, Ended> {
    let verify = {
        let control = Arc::clone(control);
        let emit = Arc::clone(emit);
        let host = config.host.clone();
        Arc::new(
            move |subject: String, issuer: String, fingerprint: String| {
                control.ask(&emit, &host, subject, issuer, fingerprint)
            },
        ) as Arc<tls::Ask>
    };
    tls::tls(transport, &config.host, verify)
        .map(|stream| Transport {
            kind: Kind::Secure(Box::new(stream)),
            pushback: None,
        })
        .map_err(|error| {
            if error.to_string().contains(CERTIFICATE_REJECTED) {
                Ended::new(
                    Failure::CertificateRejected,
                    "the TLS certificate was rejected",
                )
            } else {
                Ended::io(error, Failure::Tls)
            }
        })
}

/// VeNCrypt: version, sub-type, then the TLS-wrapped authentication it names.
fn vencrypt(
    transport: Transport,
    config: &Config,
    emit: &Emit,
    control: &Arc<Control>,
) -> Result<Transport, Ended> {
    let mut transport = transport;
    let mut version = [0u8; 2];
    read_exact(&mut transport, &mut version)?;
    let offered = u16::from_be_bytes(version);
    if offered < 0x0002 {
        let _ = send(&mut transport, &[0, 0]);
        return Err(Ended::new(
            Failure::UnsupportedSecurity,
            format!(
                "the server announced VeNCrypt {offered:#06x}, which this client does not speak"
            ),
        ));
    }
    send(&mut transport, &[0, 2])?;
    if read_u8(&mut transport)? != 0 {
        return Err(Ended::new(
            Failure::UnsupportedSecurity,
            "the server could not agree to VeNCrypt 0.2",
        ));
    }
    let count = read_u8(&mut transport)?;
    let mut types = vec![0u8; usize::from(count) * 4];
    read_exact(&mut transport, &mut types)?;
    let subtypes: Vec<u32> = types
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| u32::from_be_bytes(*chunk))
        .collect();
    super::trace(&format!("VeNCrypt sub-types {subtypes:?}"));
    // Encrypted authentication with the VNC password is the strongest form this
    // client can complete; TLS with no credentials is next. TigerVNC numbers its
    // sub-types 256 and up while the VeNCrypt specification numbers them 2 and 3,
    // so both forms of the two names are accepted.
    // TLSVnc (3, or TigerVNC's 258) and X509Vnc (6, or 261) carry the VNC
    // password inside TLS; their None forms carry no credentials. The anonymous
    // TLS forms (TLSNone/TLSVnc as TigerVNC names the anon-DH family) cannot be
    // completed by a client that requires a certificate.
    let rank = |kind: u32| -> Option<u8> {
        if !config.password.is_empty() && matches!(kind, 3 | 258 | 6 | 261) {
            Some(0)
        } else if matches!(kind, 2 | 257 | 5 | 260) {
            Some(1)
        } else {
            None
        }
    };
    let chosen = subtypes
        .iter()
        .copied()
        .filter(|kind| rank(*kind).is_some())
        .min_by_key(|kind| rank(*kind).unwrap_or(u8::MAX));
    let Some(chosen) = chosen else {
        return Err(Ended::new(
            Failure::UnsupportedSecurity,
            format!(
                "the server offers VeNCrypt sub-types this client cannot complete ({})",
                subtypes
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        ));
    };
    send(&mut transport, &chosen.to_be_bytes())?;
    let vnc_auth = matches!(chosen, 3 | 258 | 6 | 261);
    // TigerVNC marks the TLS upgrade with a 1 byte here; a server that starts the
    // handshake directly sends a TLS record type instead, which is put back.
    let marker = read_u8(&mut transport)?;
    if marker != 1 {
        transport.push_back(marker);
    }
    let mut inner = secure(transport, config, emit, control)?;
    if vnc_auth {
        authenticate(&mut inner, &config.password)?;
    }
    result(&mut inner, 8)?;
    Ok(inner)
}

fn server_init(transport: &mut Transport) -> Result<(u16, u16), Ended> {
    let width = read_u16(transport)?;
    let height = read_u16(transport)?;
    // The pixel format follows; it is replaced by the one this client asks for.
    let mut format = [0u8; 16];
    read_exact(transport, &mut format)?;
    let _name = read_string(transport)?;
    Ok((width, height))
}

fn send(transport: &mut Transport, bytes: &[u8]) -> Result<(), Ended> {
    transport
        .write_all(bytes)
        .and_then(|()| transport.flush())
        .map_err(|error| Ended::io(error, Failure::Io))
}

fn read_u8(transport: &mut Transport) -> Result<u8, Ended> {
    let mut value = [0u8; 1];
    read_exact(transport, &mut value)?;
    Ok(value[0])
}

fn read_u16(transport: &mut Transport) -> Result<u16, Ended> {
    let mut value = [0u8; 2];
    read_exact(transport, &mut value)?;
    Ok(u16::from_be_bytes(value))
}

fn read_u32(transport: &mut Transport) -> Result<u32, Ended> {
    let mut value = [0u8; 4];
    read_exact(transport, &mut value)?;
    Ok(u32::from_be_bytes(value))
}

/// A length-prefixed string, bounded so a broken length cannot allocate.
fn read_string(transport: &mut Transport) -> Result<String, Ended> {
    let length = read_u32(transport)? as usize;
    if length > 4096 {
        return Err(Ended::new(
            Failure::ProtocolFailure,
            format!("the server sent a {length} byte string where a short one belongs"),
        ));
    }
    let mut bytes = vec![0u8; length];
    read_exact(transport, &mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_password_is_reversed_and_used_as_a_des_key() {
        let challenge = [
            0x00u8, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let first = response("password", &challenge);
        assert_ne!(&first[..8], &challenge[..8], "the challenge is encrypted");
        // Eight bytes fill the key; longer is cut and shorter is zero padded, as
        // the server does on its side.
        assert_eq!(
            response("password", &challenge),
            response("passwordlong", &challenge)
        );
        assert_ne!(
            response("password", &challenge),
            response("passwor", &challenge)
        );
    }
}
