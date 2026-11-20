//! TLS as a transport for the protocol clients: a rustls stream whose trust
//! decision is the user's.
//!
//! Both RDP and VNC wrap their socket with this. The platform trust store decides
//! whether a certificate is already trusted; only an untrusted or changed one
//! becomes a question, and a question that is not answered in time is a refusal.
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};
use sha2::{Digest, Sha256};
use std::fmt;
use std::io::{self, Read, Write};
use std::sync::Arc;
use x509_parser::prelude::{FromDer, X509Certificate};

/// A decoded TLS stream over S; reads and writes go through the connection.
pub(crate) type SecureStream<S> = StreamOwned<ClientConnection, S>;

/// The verifier reports a refusal with this text, so a session can tell a rejected
/// certificate from a broken handshake.
pub(crate) const CERTIFICATE_REJECTED: &str = "certificate rejected";

/// Asks the host whether to trust a certificate: subject, issuer, fingerprint.
pub(crate) type Ask = dyn Fn(String, String, String) -> bool + Send + Sync;

struct Trust {
    platform: rustls_platform_verifier::Verifier,
    ask: Arc<Ask>,
}

impl fmt::Debug for Trust {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Shellune certificate verifier")
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
                    .map(|value| format!("{value:02x}"))
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

/// Wraps the socket in TLS and completes the handshake before returning. The
/// server name is the one the user configured, which is also what the certificate
/// is checked against.
pub(crate) fn tls<S: Read + Write>(
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
    let name = ServerName::try_from(host.to_owned())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid TLS server name"))?;
    let mut stream = StreamOwned::new(
        ClientConnection::new(Arc::new(config), name).map_err(io::Error::other)?,
        socket,
    );
    while stream.conn.is_handshaking() {
        stream.conn.complete_io(&mut stream.sock)?;
    }
    Ok(stream)
}
