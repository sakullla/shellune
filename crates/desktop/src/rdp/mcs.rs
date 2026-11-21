//! T.125 MCS connection and channel multiplexing, [MS-RDPBCGR] 2.2.1.3–11.
use super::wire::{Packet, Security, invalid, per_length, read_data, write_data};
use std::io::{self, Read, Write};

const T124_OID: &[u8] = &[0, 5, 0, 20, 124, 0, 1];
const BASE_USER_ID: u16 = 1001;

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
fn core(width: u16, height: u16, selected: Security) -> Vec<u8> {
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
    body.extend_from_slice(&32u16.to_le_bytes()); // post-beta2 colour depth, in bits
    body.extend_from_slice(&1u16.to_le_bytes()); // client product id
    body.extend_from_slice(&0u32.to_le_bytes()); // serial number
    body.extend_from_slice(&32u16.to_le_bytes()); // high colour depth
    body.extend_from_slice(&0x000fu16.to_le_bytes()); // 15, 16, 24 and 32 bit depths
    // Error-info PDUs and a 32-bit session are what this client renders.
    body.extend_from_slice(&0x0003u16.to_le_bytes());
    body.extend_from_slice(&[0; 64]); // client digital product id
    body.push(6); // connection type: broadband
    body.push(0); // pad
    body.extend_from_slice(&(selected as u32).to_le_bytes());
    // Physical size in millimetres at 96 dpi, so a resize request carries a sane
    // monitory hint instead of zero.
    body.extend_from_slice(
        &u32::from(width)
            .saturating_mul(254)
            .div_euclid(960)
            .to_le_bytes(),
    );
    body.extend_from_slice(
        &u32::from(height)
            .saturating_mul(254)
            .div_euclid(960)
            .to_le_bytes(),
    );
    body.extend_from_slice(&0u16.to_le_bytes()); // landscape
    body.extend_from_slice(&100u32.to_le_bytes()); // desktop scale factor, percent
    body.extend_from_slice(&100u32.to_le_bytes()); // device scale factor, percent
    debug_assert_eq!(body.len(), 230);
    body
}

fn connect_initial(
    width: u16,
    height: u16,
    selected: Security,
    names: &[&[u8; 8]],
) -> io::Result<Vec<u8>> {
    let mut blocks = Vec::new();
    block(&mut blocks, 0xc001, &core(width, height, selected));
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
}

fn server_channels(bytes: &[u8], count: usize) -> io::Result<(u16, Vec<u16>)> {
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
    let mut secure = false;
    while !fields.0.is_empty() {
        let kind = fields.le16()?;
        let len = fields
            .le16()?
            .checked_sub(4)
            .ok_or_else(|| invalid("short GCC block"))?;
        let mut field = Packet(fields.take(usize::from(len))?);
        match kind {
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
    network.ok_or_else(|| invalid("missing server network settings"))
}

pub(super) fn connect(
    io: &mut (impl Read + Write),
    width: u16,
    height: u16,
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
    write_data(io, &connect_initial(width, height, security, &names)?)?;
    let (global, ids) = server_channels(&read_data(io)?, names.len())?;
    write_data(io, &[4, 1, 0, 1, 0])?; // ErectDomainRequest
    write_data(io, &[0x28])?; // AttachUserRequest
    let attach = read_data(io)?;
    let mut a = Packet(&attach);
    a.expect(&[0x2e, 0])?;
    let offset = a.be16()?;
    a.finish()?;
    let user = offset
        .checked_add(BASE_USER_ID)
        .ok_or_else(|| invalid("invalid MCS user"))?;
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
    let mut ids = ids.into_iter();
    Ok(Channels {
        user,
        global,
        clipboard: if clipboard { ids.next() } else { None },
        drive: if drive { ids.next() } else { None },
    })
}

impl Channels {
    pub(super) fn send(
        &self,
        stream: &mut impl Write,
        channel: u16,
        value: &[u8],
    ) -> io::Result<()> {
        let mut message = vec![0x64];
        message.extend_from_slice(&(self.user - BASE_USER_ID).to_be_bytes());
        message.extend_from_slice(&channel.to_be_bytes());
        message.push(0x70); // high priority, begin/end
        per_length(&mut message, value.len())?;
        message.extend_from_slice(value);
        write_data(stream, &message)
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
