//! TLS and CredSSP are external security providers. The protocol's identity
//! prompt remains owned by Shellune and the certificate decision precedes login.
use super::wire::{Security, invalid};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};
use sha2::{Digest, Sha256};
use sspi::credssp::{ClientMode, ClientState, CredSspClient, CredSspMode, TsRequest};
use sspi::ntlm::NtlmConfig;
use sspi::{AuthIdentity, Credentials, Username};
use std::fmt;
use std::io::{self, Read, Write};
use std::sync::Arc;
use x509_parser::prelude::{FromDer, X509Certificate};
use zeroize::Zeroizing;

pub(super) type SecureStream<T> = StreamOwned<ClientConnection, T>;

/// The TLS verifier reports a refusal with this text, so the session can tell a
/// rejected certificate from a broken handshake.
pub(super) const CERTIFICATE_REJECTED: &str = "RDP certificate rejected";
pub(super) type Ask = dyn Fn(String, String, String) -> bool + Send + Sync;

struct Trust {
    platform: rustls_platform_verifier::Verifier,
    ask: Arc<Ask>,
}

impl fmt::Debug for Trust {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RDP certificate verifier")
    }
}

impl ServerCertVerifier for Trust {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self
            .platform
            .verify_server_cert(end_entity, intermediates, name, ocsp, now)
        {
            Err(error @ rustls::Error::InvalidCertificate(_)) => {
                let Ok((_, certificate)) = X509Certificate::from_der(end_entity.as_ref()) else {
                    return Err(error);
                };
                let fingerprint = Sha256::digest(end_entity.as_ref())
                    .iter()
                    .map(|v| format!("{v:02x}"))
                    .collect::<Vec<_>>()
                    .join(":");
                if (self.ask)(
                    certificate.subject().to_string(),
                    certificate.issuer().to_string(),
                    fingerprint,
                ) {
                    Ok(ServerCertVerified::assertion())
                } else {
                    Err(rustls::Error::General(CERTIFICATE_REJECTED.into()))
                }
            }
            result => result,
        }
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.platform
            .verify_tls12_signature(message, cert, signature)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.platform
            .verify_tls13_signature(message, cert, signature)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.platform.supported_verify_schemes()
    }
}

pub(super) fn tls<S: Read + Write>(
    socket: S,
    host: &str,
    ask: Arc<Ask>,
) -> io::Result<SecureStream<S>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let trust = Trust {
        platform: rustls_platform_verifier::Verifier::new(provider.clone())
            .map_err(io::Error::other)?,
        ask,
    };
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?;
    let config = builder
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(trust))
        .with_no_client_auth();
    let name =
        ServerName::try_from(host.to_owned()).map_err(|_| invalid("invalid RDP server name"))?;
    let mut stream = StreamOwned::new(
        ClientConnection::new(Arc::new(config), name).map_err(io::Error::other)?,
        socket,
    );
    while stream.conn.is_handshaking() {
        stream.conn.complete_io(&mut stream.sock)?;
    }
    Ok(stream)
}

fn read_ts_request(stream: &mut impl Read) -> io::Result<TsRequest> {
    let mut header = [0u8; 2];
    stream.read_exact(&mut header)?;
    if header[0] != 0x30 {
        return Err(invalid("invalid CredSSP message"));
    }
    let mut body = header.to_vec();
    let len = match header[1] {
        n @ 0..=127 => usize::from(n),
        n @ 0x81..=0x82 => {
            let mut bytes = [0u8; 2];
            let digits = &mut bytes[2 - usize::from(n & 127)..];
            stream.read_exact(digits)?;
            body.extend_from_slice(digits);
            usize::from(u16::from_be_bytes(bytes))
        }
        _ => return Err(invalid("invalid CredSSP message length")),
    };
    let offset = body.len();
    body.resize(offset + len, 0);
    stream.read_exact(&mut body[offset..])?;
    TsRequest::from_buffer(&body)
}

pub(super) fn authenticate<S: Read + Write>(
    stream: &mut SecureStream<S>,
    selected: Security,
    host: &str,
    user: &str,
    domain: &str,
    password: &str,
) -> io::Result<()> {
    if selected == Security::Tls {
        return Ok(());
    }
    let certificate = stream
        .conn
        .peer_certificates()
        .and_then(|chain| chain.first())
        .ok_or_else(|| invalid("missing TLS certificate for CredSSP"))?;
    let (_, parsed) = X509Certificate::from_der(certificate.as_ref())
        .map_err(|_| invalid("invalid TLS certificate for CredSSP"))?;
    let username = if domain.is_empty() {
        Username::parse(user)
    } else {
        Username::new(user, Some(domain))
    }
    .map_err(|_| invalid("invalid RDP username or domain"))?;
    let identity = Credentials::AuthIdentity(AuthIdentity {
        username,
        password: password.to_owned().into(),
    });
    let mut client = CredSspClient::new(
        parsed.public_key().subject_public_key.data.to_vec(),
        identity,
        CredSspMode::WithCredentials,
        ClientMode::Ntlm(NtlmConfig::new("Shellune".into())),
        format!("TERMSRV/{host}"),
    )
    .map_err(io::Error::other)?;
    let mut incoming = TsRequest::default();
    loop {
        let step = client
            .process(incoming)
            .resolve_to_result()
            .map_err(io::Error::other)?;
        let (outgoing, complete) = match step {
            ClientState::ReplyNeeded(request) => (request, false),
            ClientState::FinalMessage(request) => (request, true),
        };
        let mut encoded = Zeroizing::new(Vec::new());
        outgoing.encode_ts_request(&mut *encoded)?;
        stream.write_all(&encoded)?;
        stream.flush()?;
        if complete {
            break;
        }
        incoming = read_ts_request(stream)?;
    }
    if selected == Security::CredSspWithAuthorization {
        let mut result = [0u8; 4];
        stream.read_exact(&mut result)?;
        if result != [0; 4] {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "RDP user authorization denied",
            ));
        }
    }
    Ok(())
}
