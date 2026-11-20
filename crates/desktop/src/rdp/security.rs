//! CredSSP is the RDP authentication layer over the transport TLS the crate
//! provides. The protocol's identity prompt remains owned by Shellune and the
//! certificate decision precedes login.
use super::wire::{Security, invalid};
pub(super) use crate::tls::{CERTIFICATE_REJECTED, SecureStream, tls};
use sspi::credssp::{ClientMode, ClientState, CredSspClient, CredSspMode, TsRequest};
use sspi::ntlm::NtlmConfig;
use sspi::{AuthIdentity, Credentials, Username};
use std::io::{self, Read, Write};
use x509_parser::prelude::{FromDer, X509Certificate};
use zeroize::Zeroizing;

/// Re-exported so the session names one path for both the constant and the stream.
pub(super) use crate::tls::Ask;

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
