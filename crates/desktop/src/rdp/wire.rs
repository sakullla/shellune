//! TPKT/X.224 framing and the security negotiation in [MS-RDPBCGR] 2.2.1.
use std::io::{self, ErrorKind, Read, Write};

pub(super) fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

/// A TPKT length covers its four-byte header. The 16-bit bound applies before
/// allocating the packet, including during unauthenticated negotiation.
fn read_tpkt(input: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut header = [0; 4];
    input.read_exact(&mut header)?;
    if header[..2] != [3, 0] {
        return Err(invalid("invalid TPKT header"));
    }
    let length = usize::from(u16::from_be_bytes([header[2], header[3]]));
    let mut body = vec![0; length.checked_sub(4).ok_or_else(|| invalid("short TPKT"))?];
    input.read_exact(&mut body)?;
    Ok(body)
}

fn write_tpkt(out: &mut impl Write, parts: &[&[u8]]) -> io::Result<()> {
    let length = parts
        .iter()
        .try_fold(4u16, |sum, part| {
            u16::try_from(part.len())
                .ok()
                .and_then(|n| sum.checked_add(n))
        })
        .ok_or_else(|| invalid("TPKT exceeds 65535 bytes"))?;
    let mut packet = Vec::with_capacity(usize::from(length));
    packet.extend_from_slice(&[3, 0]);
    packet.extend_from_slice(&length.to_be_bytes());
    for part in parts {
        packet.extend_from_slice(part);
    }
    out.write_all(&packet)?;
    out.flush()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(super) enum Security {
    Tls = 1,
    CredSsp = 2,
    CredSspWithAuthorization = 8,
}

/// Advertise only external security protocols. Never send credentials or
/// silently switch to standard RDP security after a negotiation failure.
pub(super) fn negotiate(io: &mut (impl Read + Write), nla: bool) -> io::Result<Security> {
    let advertised = if nla { 1u32 | 2 | 8 } else { 1 };
    let request = [14, 0xe0, 0, 0, 0, 0, 0, 1, 0, 8, 0];
    write_tpkt(io, &[&request, &advertised.to_le_bytes()])?;
    let response = read_tpkt(io)?;
    if response.len() < 7
        || usize::from(response[0]) + 1 != response.len()
        || response[1] != 0xd0
        || response[6] != 0
    {
        return Err(invalid("invalid X.224 connection confirmation"));
    }
    if response.len() == 7 {
        return Err(io::Error::new(
            ErrorKind::Unsupported,
            "server only offers standard RDP security",
        ));
    }
    if response.len() != 15 || response[9..11] != [8, 0] {
        return Err(invalid("invalid RDP negotiation response length"));
    }
    let value = u32::from_le_bytes(response[11..15].try_into().unwrap());
    match (response[7], value) {
        (2, 1) => Ok(Security::Tls),
        (2, 2) if nla => Ok(Security::CredSsp),
        (2, 8) if nla => Ok(Security::CredSspWithAuthorization),
        (2, _) => Err(io::Error::new(
            ErrorKind::Unsupported,
            "server selected an unadvertised RDP security protocol",
        )),
        (3, reason) => Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!("RDP security negotiation refused (code {reason})"),
        )),
        _ => Err(invalid("unknown RDP negotiation structure")),
    }
}

pub(super) fn write_data(out: &mut impl Write, pdu: &[u8]) -> io::Result<()> {
    write_tpkt(out, &[&[2, 0xf0, 0x80], pdu])
}

pub(super) fn read_data(input: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut body = read_tpkt(input)?;
    if !body.starts_with(&[2, 0xf0, 0x80]) {
        return Err(invalid("expected X.224 data"));
    }
    body.drain(..3);
    Ok(body)
}

/// Walk one bounded PDU. Nested fields receive a subcursor so a malformed
/// length cannot make a field consume bytes belonging to its successor.
pub(super) struct Packet<'a>(pub(super) &'a [u8]);

impl<'a> Packet<'a> {
    pub(super) fn take(&mut self, count: usize) -> io::Result<&'a [u8]> {
        let (value, rest) = self
            .0
            .split_at_checked(count)
            .ok_or_else(|| invalid("truncated RDP field"))?;
        self.0 = rest;
        Ok(value)
    }
    pub(super) fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub(super) fn le16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub(super) fn le32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(super) fn be16(&mut self) -> io::Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub(super) fn expect(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.take(bytes.len())? == bytes {
            Ok(())
        } else {
            Err(invalid("unexpected RDP field"))
        }
    }
    pub(super) fn per_length(&mut self) -> io::Result<usize> {
        let hi = self.u8()?;
        if hi < 128 {
            return Ok(usize::from(hi));
        }
        Ok(usize::from(u16::from_be_bytes([hi & 0x7f, self.u8()?])))
    }
    pub(super) fn ber_value(&mut self, tag: &[u8]) -> io::Result<Packet<'a>> {
        self.expect(tag)?;
        let len = match self.u8()? {
            length @ 0..=127 => usize::from(length),
            0x81 => usize::from(self.u8()?),
            0x82 => usize::from(self.be16()?),
            _ => return Err(invalid("invalid BER length in MCS response")),
        };
        Ok(Packet(self.take(len)?))
    }
    pub(super) fn finish(&self) -> io::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(invalid("trailing data in RDP field"))
        }
    }
}

pub(super) fn per_length(target: &mut Vec<u8>, length: usize) -> io::Result<()> {
    match length {
        0..=127 => target.push(length as u8),
        128..=0x7fff => target.extend_from_slice(&(0x8000 | length as u16).to_be_bytes()),
        _ => return Err(invalid("MCS data exceeds PDU size")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A peer that answers with canned bytes and swallows what the client writes.
    struct Peer {
        reply: Vec<u8>,
        at: usize,
        written: Vec<u8>,
    }

    impl Read for Peer {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let left = self.reply.len() - self.at;
            if left == 0 {
                return Ok(0);
            }
            let take = left.min(buffer.len());
            buffer[..take].copy_from_slice(&self.reply[self.at..self.at + take]);
            self.at += take;
            Ok(take)
        }
    }

    impl Write for Peer {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(buffer);
            Ok(buffer.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn confirmation(selected: Option<u32>) -> Vec<u8> {
        // The X.224 length counts everything after its own byte.
        let mut body = vec![0, 0xd0, 0, 0, 0x12, 0x34, 0];
        if let Some(value) = selected {
            body.push(2); // RDP_NEG_RSP
            body.push(0);
            body.extend_from_slice(&8u16.to_le_bytes());
            body.extend_from_slice(&value.to_le_bytes());
        }
        body[0] = u8::try_from(body.len() - 1).unwrap();
        let mut packet = vec![3, 0];
        packet.extend_from_slice(&u16::try_from(body.len() + 4).unwrap().to_be_bytes());
        packet.extend_from_slice(&body);
        packet
    }

    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn a_complete_tpkt_uses_one_write_and_handles_a_short_write() {
        struct Writer {
            limit: usize,
            writes: usize,
            bytes: Vec<u8>,
        }
        impl Write for Writer {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.writes += 1;
                let n = buf.len().min(self.limit);
                self.bytes.extend_from_slice(&buf[..n]);
                Ok(n)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        for (limit, writes) in [(64, 1), (3, 3)] {
            let mut out = Writer {
                limit,
                writes: 0,
                bytes: Vec::new(),
            };
            write_data(&mut out, &[42, 43]).unwrap();
            assert_eq!(out.bytes, [3, 0, 0, 9, 2, 0xf0, 0x80, 42, 43]);
            assert_eq!(out.writes, writes);
        }
    }

    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn negotiation_selects_only_external_security() {
        let mut peer = Peer {
            reply: confirmation(Some(1)),
            at: 0,
            written: Vec::new(),
        };
        assert_eq!(negotiate(&mut peer, true).unwrap(), Security::Tls);
        // The client advertises TLS, CredSSP and CredSSP with authorization, and
        // never standard RDP security.
        assert!(peer.written.len() >= 11);
        let advertised =
            u32::from_le_bytes(peer.written[peer.written.len() - 4..].try_into().unwrap());
        assert_eq!(advertised & 1, 1);
        assert_eq!(advertised & !0xbu32, 0);
    }

    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn a_server_offering_only_standard_security_is_refused() {
        let mut peer = Peer {
            reply: confirmation(None),
            at: 0,
            written: Vec::new(),
        };
        let error = negotiate(&mut peer, true).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn a_negotiation_failure_carries_the_server_code() {
        let mut body = vec![0, 0xd0, 0, 0, 0, 0, 0];
        body.push(3); // RDP_NEG_FAILURE
        body.push(0);
        body.extend_from_slice(&8u16.to_le_bytes());
        body.extend_from_slice(&5u32.to_le_bytes());
        body[0] = u8::try_from(body.len() - 1).unwrap();
        let mut packet = vec![3, 0];
        packet.extend_from_slice(&u16::try_from(body.len() + 4).unwrap().to_be_bytes());
        packet.extend_from_slice(&body);
        let mut peer = Peer {
            reply: packet,
            at: 0,
            written: Vec::new(),
        };
        let error = negotiate(&mut peer, true).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains('5'));
    }
}
