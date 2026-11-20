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
/// Anonymous Diffie-Hellman TLS (TigerVNC's extension). This build has no
/// anonymous cipher suites, so it is refused instead of attempted.
const ANONYMOUS_TLS: u8 = 18;
const VENCRYPT: u8 = 19;

/// VeNCrypt sub-types, numbered as the specification numbers them.
const VENCRYPT_PLAIN: u32 = 256;
const VENCRYPT_TLS_NONE: u32 = 257;
const VENCRYPT_TLS_VNC: u32 = 258;
const VENCRYPT_TLS_PLAIN: u32 = 259;
const VENCRYPT_X509_NONE: u32 = 260;
const VENCRYPT_X509_VNC: u32 = 261;
const VENCRYPT_X509_PLAIN: u32 = 262;
const VENCRYPT_SASL_NONE: u32 = 263;
const VENCRYPT_SASL_VNC: u32 = 264;
/// LibVNCServer lists its plain security types inside VeNCrypt as well: 1 and 2
/// mean no authentication and VNC authentication on the unwrapped connection.
const VENCRYPT_COMPAT_NONE: u32 = 1;
const VENCRYPT_COMPAT_VNC: u32 = 2;

/// One VeNCrypt sub-type this client can complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SubType {
    /// TLS with a certificate, no further credentials.
    X509None,
    /// TLS with a certificate, then VNC authentication.
    X509Vnc,
    /// LibVNCServer's no-authentication form, without TLS.
    PlainNone,
    /// LibVNCServer's VNC authentication form, without TLS.
    PlainVnc,
}

impl SubType {
    fn wire(self) -> u32 {
        match self {
            SubType::X509None => VENCRYPT_X509_NONE,
            SubType::X509Vnc => VENCRYPT_X509_VNC,
            SubType::PlainNone => VENCRYPT_COMPAT_NONE,
            SubType::PlainVnc => VENCRYPT_COMPAT_VNC,
        }
    }

    fn tls(self) -> bool {
        matches!(self, SubType::X509None | SubType::X509Vnc)
    }

    fn needs_password(self) -> bool {
        matches!(self, SubType::X509Vnc | SubType::PlainVnc)
    }
}

/// Why no sub-type could be chosen.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// Every sub-type the server offers is anonymous TLS.
    Anonymous,
    /// Something else this client cannot complete.
    Unsupported,
}

/// The sentence a refusal is shown with.
pub(super) const ANONYMOUS_TLS_REASON: &str =
    "the server only offers anonymous TLS, which this build cannot provide";

/// Picks the best sub-type this client can complete.
///
/// A certificate beats the unwrapped compatibility forms, and VNC
/// authentication beats no credentials when a password is available. The
/// anonymous TLS forms (257, 258) are never candidates: they need an anonymous
/// Diffie-Hellman cipher suite, which this build does not have.
pub(super) fn choose_subtype(subtypes: &[u32], has_password: bool) -> Result<SubType, Refusal> {
    let order: &[SubType] = if has_password {
        &[
            SubType::X509Vnc,
            SubType::X509None,
            SubType::PlainVnc,
            SubType::PlainNone,
        ]
    } else {
        &[SubType::X509None, SubType::PlainNone]
    };
    for kind in order {
        if subtypes.contains(&kind.wire()) {
            return Ok(*kind);
        }
    }
    let mut anonymous = true;
    for kind in subtypes {
        if !matches!(*kind, VENCRYPT_TLS_NONE | VENCRYPT_TLS_VNC) {
            anonymous = false;
        }
    }
    if anonymous && !subtypes.is_empty() {
        Err(Refusal::Anonymous)
    } else {
        Err(Refusal::Unsupported)
    }
}

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
    let bytes = u64::from(width) * u64::from(height) * 4;
    if width == 0 || height == 0 || bytes > MAX_FRAME_BYTES as u64 {
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
        if !type_allowed(chosen, config.security) {
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
    match types
        .iter()
        .copied()
        .find(|kind| type_allowed(*kind, config.security))
    {
        Some(chosen) => {
            send(transport, &[chosen])?;
            Ok(chosen)
        }
        None => {
            // 0 names no type: the connection cannot go on, and saying so is the
            // protocol's own answer.
            let _ = send(transport, &[0]);
            let anonymous = types.iter().all(|kind| *kind == ANONYMOUS_TLS);
            Err(Ended::new(
                Failure::UnsupportedSecurity,
                if anonymous {
                    ANONYMOUS_TLS_REASON.to_owned()
                } else {
                    format!(
                        "the server offers only security types this client refuses ({})",
                        types
                            .iter()
                            .map(u8::to_string)
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                },
            ))
        }
    }
}

/// Whether the policy in `mask` allows this security type.
///
/// Anonymous TLS (18) is never allowed, even when the policy names it: this build
/// has no anonymous cipher suites, and selecting it would only fail later inside
/// the handshake.
fn type_allowed(kind: u8, mask: u32) -> bool {
    match kind {
        NONE => mask & 0x1 != 0,
        VNC_AUTH => mask & 0x2 != 0,
        VENCRYPT => mask & 0x8 != 0,
        _ => false,
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
    let kind = match choose_subtype(&subtypes, !config.password.is_empty()) {
        Ok(kind) => kind,
        Err(Refusal::Anonymous) => {
            return Err(Ended::new(
                Failure::UnsupportedSecurity,
                ANONYMOUS_TLS_REASON,
            ));
        }
        Err(Refusal::Unsupported) => {
            return Err(Ended::new(
                Failure::UnsupportedSecurity,
                format!(
                    "the server offers VeNCrypt sub-types this client cannot complete ({})",
                    describe_subtypes(&subtypes)
                ),
            ));
        }
    };
    super::trace(&format!("VeNCrypt sub-type {kind:?}"));
    send(&mut transport, &kind.wire().to_be_bytes())?;
    if kind.tls() {
        // TigerVNC marks the TLS upgrade with a 1 byte here; a server that starts
        // the handshake directly sends a TLS record type instead, which is put
        // back in front of the stream.
        let marker = read_u8(&mut transport)?;
        if marker != 1 {
            transport.push_back(marker);
        }
        let mut inner = secure(transport, config, emit, control)?;
        if kind.needs_password() {
            authenticate(&mut inner, &config.password)?;
        }
        result(&mut inner, 8)?;
        Ok(inner)
    } else {
        // LibVNCServer's unwrapped forms: the authentication and its result follow
        // directly on the connection.
        if kind.needs_password() {
            authenticate(&mut transport, &config.password)?;
        }
        result(&mut transport, 8)?;
        Ok(transport)
    }
}

/// The protocol name of a VeNCrypt sub-type, for diagnostics.
fn subtype_name(kind: u32) -> &'static str {
    match kind {
        VENCRYPT_PLAIN => "Plain",
        VENCRYPT_TLS_NONE => "TLSNone",
        VENCRYPT_TLS_VNC => "TLSVnc",
        VENCRYPT_TLS_PLAIN => "TLSPlain",
        VENCRYPT_X509_NONE => "X509None",
        VENCRYPT_X509_VNC => "X509Vnc",
        VENCRYPT_X509_PLAIN => "X509Plain",
        VENCRYPT_SASL_NONE => "SASLNone",
        VENCRYPT_SASL_VNC => "SASLVnc",
        VENCRYPT_COMPAT_NONE => "None",
        VENCRYPT_COMPAT_VNC => "VncAuth",
        _ => "unknown",
    }
}

fn describe_subtypes(subtypes: &[u32]) -> String {
    subtypes
        .iter()
        .map(|kind| format!("{kind}({})", subtype_name(*kind)))
        .collect::<Vec<_>>()
        .join(",")
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
    fn anonymous_vencrypt_sub_types_are_refused_with_a_readable_reason() {
        assert_eq!(choose_subtype(&[258], true), Err(Refusal::Anonymous));
        assert_eq!(choose_subtype(&[257], false), Err(Refusal::Anonymous));
        assert_eq!(choose_subtype(&[257, 258], true), Err(Refusal::Anonymous));
        assert!(ANONYMOUS_TLS_REASON.contains("anonymous TLS"));
        assert!(ANONYMOUS_TLS_REASON.contains("cannot provide"));
    }

    #[test]
    fn a_certificate_sub_type_wins_over_an_anonymous_one() {
        assert_eq!(choose_subtype(&[258, 261], true), Ok(SubType::X509Vnc));
        assert_eq!(choose_subtype(&[257, 260], false), Ok(SubType::X509None));
        assert_eq!(choose_subtype(&[261, 260], true), Ok(SubType::X509Vnc));
        // A VNC-authentication sub-type needs a password; without one the
        // certificate form that carries no credentials is the only usable one.
        assert_eq!(choose_subtype(&[261], false), Err(Refusal::Unsupported));
    }

    #[test]
    fn libvnc_plain_forms_are_accepted_without_tls() {
        assert_eq!(choose_subtype(&[1], false), Ok(SubType::PlainNone));
        assert_eq!(choose_subtype(&[1, 2], true), Ok(SubType::PlainVnc));
        assert_eq!(choose_subtype(&[1, 2], false), Ok(SubType::PlainNone));
        assert_eq!(choose_subtype(&[2], false), Err(Refusal::Unsupported));
        assert_eq!(choose_subtype(&[1, 260], true), Ok(SubType::X509None));
    }

    #[test]
    fn the_old_numbering_is_not_read_as_a_password_type() {
        // 3 and 6 are not VeNCrypt sub-types; treating them as TLSVnc/X509Vnc
        // would select a number the server does not offer.
        assert_eq!(choose_subtype(&[3, 6], true), Err(Refusal::Unsupported));
        assert_eq!(
            choose_subtype(&[256, 259, 262, 263, 264], true),
            Err(Refusal::Unsupported)
        );
    }

    #[test]
    fn the_standalone_anonymous_tls_type_is_never_allowed() {
        for mask in [0x4, 0x4 | 0x8, 0xC] {
            assert!(!type_allowed(ANONYMOUS_TLS, mask), "mask {mask:#x}");
        }
        assert!(type_allowed(VENCRYPT, 0x8));
        assert!(type_allowed(NONE, 0x1));
        assert!(type_allowed(VNC_AUTH, 0x2));
    }

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
