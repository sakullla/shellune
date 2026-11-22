//! T.125 MCS connection and channel multiplexing, [MS-RDPBCGR] 2.2.1.3–11.
use super::wire::{Packet, Security, invalid, per_length, read_data, write_data};
use std::io::{self, Read, Write};

const T124_OID: &[u8] = &[0, 5, 0, 20, 124, 0, 1];
const BASE_USER_ID: u16 = 1001;
const SUPPORT_SKIP_CHANNEL_JOIN: u16 = 0x0800;
const SKIP_CHANNEL_JOIN_SUPPORTED: u32 = 0x0008;

fn ber(out: &mut Vec<u8>, tag: &[u8], value: &[u8]) {
    out.extend_from_slice(tag);
    match value.len() {
        0..128 => out.push(value.len() as u8),
        128..256 => out.extend_from_slice(&[0x81, value.len() as u8]),
        _ => {
            out.push(0x82);
            out.extend_from_slice(&(value.len() as u16).to_be_bytes());
        }
    }
    out.extend_from_slice(value);
}

fn domain_parameters(out: &mut Vec<u8>, values: [u16; 8]) {
    let mut body = Vec::new();
    for value in values {
        let bytes = value.to_be_bytes();
        let digits = if bytes[0] == 0 {
            &bytes[1..]
        } else {
            &bytes[..]
        };
        ber(&mut body, &[2], digits);
    }
    ber(out, &[0x30], &body);
}

fn block(out: &mut Vec<u8>, kind: u16, value: &[u8]) {
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&((value.len() + 4) as u16).to_le_bytes());
    out.extend_from_slice(value);
}

/// TS_UD_CS_CORE ([MS-RDPBCGR] 2.2.1.3.2). Every field is written at the offset
/// the protocol defines, because the server reads this block positionally: a
/// shifted field is read as the next one, which is how a client ends up with a
/// desktop of 0 pixels and no picture.
fn core(width: u16, height: u16, percent: u32, selected: Security) -> Vec<u8> {
    let mut body = Vec::with_capacity(230);
    body.extend_from_slice(&0x0008_0004u32.to_le_bytes()); // RDP 5.0+
    body.extend_from_slice(&width.to_le_bytes());
    body.extend_from_slice(&height.to_le_bytes());
    body.extend_from_slice(&0xca01u16.to_le_bytes()); // colour depth placeholder
    body.extend_from_slice(&0xaa03u16.to_le_bytes()); // SAS sequence
    body.extend_from_slice(&0x409u32.to_le_bytes()); // US keyboard layout
    body.extend_from_slice(&1u32.to_le_bytes()); // Shellune build identifier
    let mut name = [0u8; 32];
    for (slot, unit) in name
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip("Shellune".encode_utf16())
    {
        slot.copy_from_slice(&unit.to_le_bytes());
    }
    body.extend_from_slice(&name);
    // These three are written as 32-bit fields: that is what the servers and the
    // reference client both read.
    body.extend_from_slice(&4u32.to_le_bytes()); // IBM enhanced (101/104 key)
    body.extend_from_slice(&0u32.to_le_bytes()); // keyboard subtype
    body.extend_from_slice(&12u32.to_le_bytes()); // function keys
    body.extend_from_slice(&[0; 64]); // imeFileName
    body.extend_from_slice(&0xca04u16.to_le_bytes()); // RNS_UD_COLOR_24BPP
    body.extend_from_slice(&1u16.to_le_bytes()); // client product id
    body.extend_from_slice(&0u32.to_le_bytes()); // serial number
    body.extend_from_slice(&24u16.to_le_bytes()); // 32 bpp is requested in earlyCapabilityFlags
    body.extend_from_slice(&0x000fu16.to_le_bytes()); // 15, 16, 24 and 32 bit depths
    // Error-info PDUs and a 32-bit session are what this client renders.
    body.extend_from_slice(&(0x0003 | SUPPORT_SKIP_CHANNEL_JOIN).to_le_bytes());
    body.extend_from_slice(&[0; 64]); // client digital product id
    body.push(6); // connection type: broadband
    body.push(0); // pad
    body.extend_from_slice(&(selected as u32).to_le_bytes());
    for pixels in [width, height] {
        body.extend_from_slice(
            &super::display::physical_mm(u32::from(pixels), percent).to_le_bytes(),
        );
    }
    body.extend_from_slice(&0u16.to_le_bytes()); // landscape
    body.extend_from_slice(&percent.to_le_bytes());
    body.extend_from_slice(&super::display::device_scale(percent).to_le_bytes());
    debug_assert_eq!(body.len(), 230);
    body
}

fn connect_initial(
    width: u16,
    height: u16,
    percent: u32,
    selected: Security,
    names: &[&[u8; 8]],
) -> io::Result<Vec<u8>> {
    let mut blocks = Vec::new();
    block(&mut blocks, 0xc001, &core(width, height, percent, selected));
    block(&mut blocks, 0xc002, &[0; 8]); // enhanced security sets both encryption fields to zero
    if !names.is_empty() {
        let mut net = (names.len() as u32).to_le_bytes().to_vec();
        for name in names {
            net.extend_from_slice(&name[..]);
            net.extend_from_slice(&0x8000_0000u32.to_le_bytes()); // initialized; no bulk compression
        }
        block(&mut blocks, 0xc003, &net);
    }
    let mut conference = vec![0, 8, 0, 16, 0, 1, 0xc0, 0];
    conference.extend_from_slice(b"Duca");
    per_length(&mut conference, blocks.len())?;
    conference.extend_from_slice(&blocks);
    let mut gcc = T124_OID.to_vec();
    per_length(&mut gcc, conference.len())?;
    gcc.extend_from_slice(&conference);

    let mut body = Vec::new();
    ber(&mut body, &[4], &[1]); // calling/called domain selectors
    ber(&mut body, &[4], &[1]);
    ber(&mut body, &[1], &[0xff]); // upward flag
    domain_parameters(&mut body, [34, 2, 0, 1, 0, 1, 65535, 2]);
    domain_parameters(&mut body, [1, 1, 1, 1, 0, 1, 1056, 2]);
    domain_parameters(&mut body, [65535, 64535, 65535, 1, 0, 1, 65535, 2]);
    ber(&mut body, &[4], &gcc);
    let mut pdu = Vec::new();
    ber(&mut pdu, &[0x7f, 0x65], &body);
    Ok(pdu)
}

/// Concrete IDs assigned by the peer; static channel IDs correspond to the
/// requested names in order and must never be guessed from the global ID.
pub(super) struct Channels {
    pub(super) user: u16,
    pub(super) global: u16,
    pub(super) clipboard: Option<u16>,
    pub(super) drive: Option<u16>,
    pub(super) dynamic: Option<u16>,
}

struct ServerChannels {
    global: u16,
    ids: Vec<u16>,
    skip_join: bool,
}

fn server_channels(bytes: &[u8], count: usize) -> io::Result<ServerChannels> {
    let mut p = Packet(bytes);
    let mut body = p.ber_value(&[0x7f, 0x66])?;
    p.finish()?;
    if body.ber_value(&[10])?.0 != [0] {
        return Err(invalid("server refused MCS connection"));
    }
    body.ber_value(&[2])?; // calledConnectId
    body.ber_value(&[0x30])?; // domainParameters
    let mut gcc = body.ber_value(&[4])?;
    body.finish()?;
    gcc.expect(T124_OID)?;
    gcc.per_length()?; // per MS-RDPBCGR the outer GCC reply length is ignored
    gcc.expect(&[0x14])?;
    gcc.take(2)?; // node ID
    gcc.expect(&[1, 1, 0, 1, 0xc0, 0])?;
    gcc.expect(b"McDn")?;
    let len = gcc.per_length()?;
    let mut fields = Packet(gcc.take(len)?);
    gcc.finish()?;
    let mut network = None;
    let mut skip_join = false;
    let mut secure = false;
    while !fields.0.is_empty() {
        let kind = fields.le16()?;
        let len = fields
            .le16()?
            .checked_sub(4)
            .ok_or_else(|| invalid("short GCC block"))?;
        let mut field = Packet(fields.take(usize::from(len))?);
        match kind {
            0x0c01 => {
                field.le32()?; // version
                if !field.0.is_empty() {
                    field.le32()?;
                } // optional clientRequestedProtocols
                if !field.0.is_empty() {
                    skip_join = field.le32()? & SKIP_CHANNEL_JOIN_SUPPORTED != 0;
                }
            }
            0x0c02 => {
                if field.le32()? != 0 || field.le32()? != 0 {
                    return Err(invalid("server selected standard security inside TLS"));
                }
                secure = true;
            }
            0x0c03 => {
                let global = field.le16()?;
                let size = usize::from(field.le16()?);
                if size != count || network.is_some() {
                    return Err(invalid("unexpected static channel count"));
                }
                let mut ids = Vec::new();
                for _ in 0..size {
                    let id = field.le16()?;
                    if global == id || ids.contains(&id) {
                        return Err(invalid("duplicate MCS channel ID"));
                    }
                    ids.push(id);
                }
                network = Some((global, ids));
            }
            _ => {}
        }
    }
    if !secure {
        return Err(invalid("missing server security settings"));
    }
    let (global, ids) = network.ok_or_else(|| invalid("missing server network settings"))?;
    Ok(ServerChannels {
        global,
        ids,
        skip_join,
    })
}

pub(super) fn connect(
    io: &mut (impl Read + Write),
    width: u16,
    height: u16,
    percent: u32,
    security: Security,
    clipboard: bool,
    drive: bool,
) -> io::Result<Channels> {
    let mut names = Vec::new();
    if clipboard {
        names.push(b"cliprdr\0");
    }
    if drive {
        names.push(b"rdpdr\0\0\0");
    }
    names.push(b"drdynvc\0");
    write_data(
        io,
        &connect_initial(width, height, percent, security, &names)?,
    )?;
    let ServerChannels {
        global,
        ids,
        skip_join,
    } = server_channels(&read_data(io)?, names.len())?;
    let mut attach_request = Vec::new();
    write_data(&mut attach_request, &[4, 1, 0, 1, 0])?; // ErectDomainRequest
    write_data(&mut attach_request, &[0x28])?; // AttachUserRequest
    io.write_all(&attach_request)?;
    io.flush()?;
    let attach = read_data(io)?;
    let mut a = Packet(&attach);
    a.expect(&[0x2e, 0])?;
    let offset = a.be16()?;
    a.finish()?;
    let user = offset
        .checked_add(BASE_USER_ID)
        .ok_or_else(|| invalid("invalid MCS user"))?;
    // With bilateral skip-channel-join support, Attach User Confirm also
    // confirms membership of the static, global and user channels.
    if !skip_join {
        // MS-RDPBCGR 1.3.1.1: all channel IDs are known after Attach User.
        // Send the joins in one TLS write, then verify every confirmation before logon.
        let base_channels = [user, global];
        let channels = base_channels.iter().chain(&ids);
        let mut batch = Vec::new();
        for id in channels.clone() {
            let mut join = vec![0x38];
            join.extend_from_slice(&offset.to_be_bytes());
            join.extend_from_slice(&id.to_be_bytes());
            write_data(&mut batch, &join)?;
        }
        io.write_all(&batch)?;
        io.flush()?;
        for id in channels {
            let response = read_data(io)?;
            let mut joined = Packet(&response);
            joined.expect(&[0x3e, 0])?;
            if joined.be16()? != offset || joined.be16()? != *id || joined.be16()? != *id {
                return Err(invalid("server confirmed a different MCS channel"));
            }
            joined.finish()?;
        }
    }
    let mut ids = ids.into_iter();
    Ok(Channels {
        user,
        global,
        clipboard: if clipboard { ids.next() } else { None },
        drive: if drive { ids.next() } else { None },
        dynamic: ids.next(),
    })
}

impl Channels {
    /// Static virtual channel PDUs use a total length and FIRST/LAST flags
    /// on each chunk (MS-RDPBCGR 2.2.6.1). No channel compression is offered.
    pub(super) fn send_channel(
        &self,
        out: &mut impl Write,
        id: u16,
        header: &[u8],
        data: &[u8],
    ) -> io::Result<()> {
        const CHUNK: usize = 1600;
        let length = u32::try_from(header.len() + data.len())
            .map_err(|_| invalid("channel PDU too large"))?;
        let mut source = header.chain(data);
        let mut remaining = length as usize;
        let mut packet = [0u8; CHUNK + 8];
        packet[..4].copy_from_slice(&length.to_le_bytes());
        while remaining > 0 {
            let n = remaining.min(CHUNK);
            let flags = u32::from(remaining == length as usize) | (u32::from(n == remaining) << 1);
            packet[4..8].copy_from_slice(&flags.to_le_bytes());
            source.read_exact(&mut packet[8..8 + n])?;
            self.send(out, id, &packet[..8 + n])?;
            remaining -= n;
        }
        Ok(())
    }

    pub(super) fn send(
        &self,
        stream: &mut impl Write,
        channel: u16,
        value: &[u8],
    ) -> io::Result<()> {
        // One TLS write per outbound PDU, including pointer and keyboard input.
        let mut message = zeroize::Zeroizing::new(Vec::with_capacity(15 + value.len()));
        message.extend_from_slice(&[3, 0, 0, 0, 2, 0xf0, 0x80, 0x64]);
        message.extend_from_slice(&(self.user - BASE_USER_ID).to_be_bytes());
        message.extend_from_slice(&channel.to_be_bytes());
        message.push(0x70); // high priority, begin/end
        per_length(&mut message, value.len())?;
        message.extend_from_slice(value);
        let length =
            u16::try_from(message.len()).map_err(|_| invalid("TPKT exceeds 65535 bytes"))?;
        message[2..4].copy_from_slice(&length.to_be_bytes());
        stream.write_all(&message)?;
        stream.flush()
    }

    pub(super) fn receive<'a>(&self, pdu: &'a [u8]) -> io::Result<(u16, &'a [u8])> {
        let mut p = Packet(pdu);
        if p.u8()? != 0x68 {
            return Err(invalid("expected MCS SendDataIndication"));
        }
        p.take(2)?; // sending user
        let id = p.be16()?;
        if p.u8()? & 0x30 != 0x30 {
            return Err(invalid("segmented MCS data is unsupported"));
        }
        let len = p.per_length()?;
        let body = p.take(len)?;
        p.finish()?;
        Ok((id, body))
    }
}

#[cfg(test)]
mod scale_tests {
    use super::*;
    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn retina_core_data_has_physical_pixels_and_scaled_dpi() {
        let block = core(2240, 1348, 200, Security::Tls);
        assert_eq!(&block[4..6], &2240u16.to_le_bytes());
        assert_eq!(&block[6..8], &1348u16.to_le_bytes());
        assert_eq!(&block[212..216], &296u32.to_le_bytes());
        assert_eq!(&block[222..226], &200u32.to_le_bytes());
        assert_eq!(&block[226..230], &180u32.to_le_bytes());
    }
}

#[cfg(test)]
mod join_tests {
    use super::*;

    struct Peer {
        replies: io::Cursor<Vec<u8>>,
        sent: Vec<u8>,
    }

    impl Read for Peer {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.replies.read(out)
        }
    }
    impl Write for Peer {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.sent.write(data)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn reply(flags: Option<u32>) -> io::Result<Vec<u8>> {
        let mut fields = Vec::new();
        let mut core = 0x0008_0004u32.to_le_bytes().to_vec();
        if let Some(flags) = flags {
            core.extend_from_slice(&1u32.to_le_bytes());
            core.extend_from_slice(&flags.to_le_bytes());
        }
        block(&mut fields, 0x0c01, &core);
        block(&mut fields, 0x0c02, &[0; 8]);
        let network: Vec<_> = [1003u16, 2, 1004, 1005]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        block(&mut fields, 0x0c03, &network);
        let mut gcc = T124_OID.to_vec();
        gcc.extend_from_slice(&[42, 0x14, 0x76, 0x0a, 1, 1, 0, 1, 0xc0, 0]);
        gcc.extend_from_slice(b"McDn");
        per_length(&mut gcc, fields.len())?;
        gcc.extend_from_slice(&fields);
        let mut content = Vec::new();
        ber(&mut content, &[10], &[0]);
        ber(&mut content, &[2], &[0]);
        ber(&mut content, &[0x30], &[]);
        ber(&mut content, &[4], &gcc);
        let mut response = Vec::new();
        ber(&mut response, &[0x7f, 0x66], &content);
        Ok(response)
    }

    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn skip_joins_only_when_the_server_acknowledges_the_client_flag() -> io::Result<()> {
        let advertised = core(800, 600, 100, Security::Tls);
        assert_ne!(
            u16::from_le_bytes(advertised[140..142].try_into().unwrap()) & 0x0800,
            0
        );
        for flags in [None, Some(0x4), Some(0x8)] {
            let skip = flags == Some(8);
            let mut replies = Vec::new();
            write_data(&mut replies, &reply(flags)?)?;
            write_data(&mut replies, &[0x2e, 0, 0, 5])?;
            let ids = [1006u16, 1003, 1004, 1005];
            if !skip {
                for id in ids {
                    let mut confirm = vec![0x3e, 0, 0, 5];
                    confirm.extend_from_slice(&id.to_be_bytes());
                    confirm.extend_from_slice(&id.to_be_bytes());
                    write_data(&mut replies, &confirm)?;
                }
            }
            let mut peer = Peer {
                replies: io::Cursor::new(replies),
                sent: Vec::new(),
            };
            let ch = connect(&mut peer, 800, 600, 100, Security::Tls, true, false)?;
            assert_eq!(
                (ch.user, ch.global, ch.clipboard, ch.dynamic),
                (1006, 1003, Some(1004), Some(1005))
            );
            let mut wire = peer.sent.as_slice();
            read_data(&mut wire)?; // Connect Initial
            assert_eq!(read_data(&mut wire)?, [4, 1, 0, 1, 0]);
            assert_eq!(read_data(&mut wire)?, [0x28]);
            if !skip {
                for id in ids {
                    let [hi, lo] = id.to_be_bytes();
                    assert_eq!(read_data(&mut wire)?, [0x38, 0, 5, hi, lo]);
                }
            }
            assert!(wire.is_empty());
        }
        Ok(())
    }
}
