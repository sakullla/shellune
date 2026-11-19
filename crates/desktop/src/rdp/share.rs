//! Desktop activation, slow-path input and Share Control/Data PDUs.
use super::mcs::Channels;
use super::wire::{Packet, Security, invalid};
use std::io::{self, Write};
use zeroize::Zeroizing;

fn words(values: &[u16]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn utf16(value: &str) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(value.encode_utf16().flat_map(|v| v.to_le_bytes()).collect())
}

pub(super) fn login(
    ch: &Channels,
    out: &mut impl Write,
    user: &str,
    domain: &str,
    password: &str,
    security: Security,
) -> io::Result<()> {
    let auto = !user.is_empty() && !password.is_empty() && security == Security::Tls;
    // INFO_MOUSE, INFO_UNICODE, INFO_LOGONNOTIFY, INFO_ENABLEWINDOWSKEY,
    // INFO_LOGONERRORS and INFO_MOUSE_HAS_WHEEL; INFO_AUTOLOGON is what makes the
    // credentials count. INFO_COMPRESSION stays off: the client renders raw
    // rectangles and would have to refuse compressed output.
    let flags = 0x0003_0173u32 | if auto { 8 } else { 0 };
    let mut pdu = Zeroizing::new(vec![0x40, 0, 0, 0]); // SEC_INFO_PKT
    pdu.extend_from_slice(&0x409u32.to_le_bytes());
    pdu.extend_from_slice(&flags.to_le_bytes());
    let fields = [domain, user, if auto { password } else { "" }, "", ""].map(utf16);
    for field in &fields {
        let length = u16::try_from(field.len()).map_err(|_| invalid("RDP login field too long"))?;
        pdu.extend_from_slice(&length.to_le_bytes());
    }
    for field in fields {
        pdu.extend_from_slice(&field);
        pdu.extend_from_slice(&[0, 0]);
    }
    pdu.extend_from_slice(&2u16.to_le_bytes()); // IPv4 address family
    for field in ["0.0.0.0", "Shellune"] {
        let encoded = utf16(field);
        pdu.extend_from_slice(&((encoded.len() + 2) as u16).to_le_bytes());
        pdu.extend_from_slice(&encoded);
        pdu.extend_from_slice(&[0, 0]);
    }
    pdu.extend_from_slice(&[0; 172]); // UTC time-zone information
    pdu.extend_from_slice(&[0; 4]); // clientSessionId
    pdu.extend_from_slice(&0x0000_000fu32.to_le_bytes()); // reduce bandwidth for classic bitmap desktops
    pdu.extend_from_slice(&[0; 2]); // no reconnect cookie
    ch.send(out, ch.global, &pdu)
}

struct Capabilities {
    count: u16,
    bytes: Vec<u8>,
}

impl Capabilities {
    fn add(&mut self, kind: u16, bytes: &[u8]) {
        self.count += 1;
        self.bytes.extend_from_slice(&kind.to_le_bytes());
        self.bytes
            .extend_from_slice(&((bytes.len() + 4) as u16).to_le_bytes());
        self.bytes.extend_from_slice(bytes);
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Share {
    id: u32,
    pub(super) width: u16,
    pub(super) height: u16,
    bpp: u16,
    server: u16,
}

pub(super) struct Control<'a> {
    pub(super) kind: u16,
    pub(super) source: u16,
    pub(super) body: &'a [u8],
}

pub(super) fn control<'a>(p: &mut Packet<'a>) -> io::Result<Control<'a>> {
    let len = p
        .le16()?
        .checked_sub(6)
        .ok_or_else(|| invalid("short Share Control header"))?;
    let kind = p.le16()?;
    if kind & 0xfff0 != 0x10 {
        return Err(invalid("unsupported Share Control version"));
    }
    Ok(Control {
        kind: kind & 15,
        source: p.le16()?,
        body: p.take(usize::from(len))?,
    })
}

impl Share {
    /// A placeholder before the server's Demand Active is read; it holds only the
    /// requested size and must never be used to send share data.
    pub(super) fn pending(width: u16, height: u16) -> Share {
        Share {
            id: 0,
            width,
            height,
            bpp: 32,
            server: 0,
        }
    }

    pub(super) fn bpp(&self) -> u16 {
        self.bpp
    }

    pub(super) fn from_demand(control: &Control<'_>) -> io::Result<Self> {
        let mut p = Packet(control.body);
        let id = p.le32()?;
        let desc = p.le16()?;
        let capability_size = p.le16()?;
        p.take(usize::from(desc))?;
        let mut caps = Packet(p.take(usize::from(capability_size))?);
        let count = caps.le16()?;
        caps.take(2)?;
        let mut desktop = None;
        for _ in 0..count {
            let kind = caps.le16()?;
            let length = caps
                .le16()?
                .checked_sub(4)
                .ok_or_else(|| invalid("short capability set"))?;
            let mut cap = Packet(caps.take(usize::from(length))?);
            if kind == 2 {
                let bpp = cap.le16()?;
                cap.take(6)?;
                let width = cap.le16()?;
                let height = cap.le16()?;
                if !matches!(bpp, 15 | 16 | 24 | 32)
                    || width == 0
                    || height == 0
                    || u64::from(width) * u64::from(height) * 4 > crate::MAX_FRAME_BYTES as u64
                {
                    return Err(invalid(
                        "unsupported desktop bitmap dimensions or colour depth",
                    ));
                }
                desktop = Some((width, height, bpp));
            }
        }
        caps.finish()?;
        let (width, height, bpp) =
            desktop.ok_or_else(|| invalid("server has no bitmap capability"))?;
        Ok(Self {
            id,
            width,
            height,
            bpp,
            server: control.source,
        })
    }

    pub(super) fn activate(&self, ch: &Channels, out: &mut impl Write) -> io::Result<()> {
        let mut caps = Capabilities {
            count: 0,
            bytes: Vec::new(),
        };
        // Slow-path updates and keyboard input. Only advertise orders/codecs that
        // can actually be rendered; the server falls back to bitmap rectangles.
        caps.add(1, &words(&[0, 0, 0x200, 0, 0, 0, 0, 0, 0, 0]));
        // Bitmap capability: 32-bit rectangles, resizable desktop, and compression
        // explicitly off — this client renders raw rectangles only, so asking for
        // RLE and then refusing it would break every update.
        caps.add(
            2,
            &words(&[self.bpp, 1, 1, 1, self.width, self.height, 0, 1, 0, 0, 1, 0]),
        );
        let mut orders = vec![0; 84];
        orders[20..32].copy_from_slice(&words(&[1, 20, 0, 1, 0, 2]));
        caps.add(3, &orders);
        caps.add(8, &words(&[1, 20, 20])); // colour pointer cache and its size
        caps.add(9, &words(&[0, 0])); // share
        let mut input = words(&[1, 0]); // scan code input
        for v in [0x409u32, 4, 0, 12] {
            input.extend_from_slice(&v.to_le_bytes());
        }
        input.extend_from_slice(&[0; 64]);
        caps.add(13, &input);
        caps.add(14, &words(&[1, 0])); // font list
        caps.add(20, &0u32.to_le_bytes()); // static virtual channels, no compression
        let mut confirm = self.id.to_le_bytes().to_vec();
        confirm.extend_from_slice(&words(&[1002, 8, (caps.bytes.len() + 4) as u16]));
        confirm.extend_from_slice(b"Shellune");
        confirm.extend_from_slice(&words(&[caps.count, 0]));
        confirm.extend_from_slice(&caps.bytes);
        self.send_control(ch, out, 3, &confirm)?;
        self.send_data(ch, out, 31, &words(&[1, self.server]))?;
        self.send_data(ch, out, 20, &words(&[4, 0, 0, 0]))?; // cooperate
        self.send_data(ch, out, 20, &words(&[1, 0, 0, 0]))?; // request control
        self.send_data(ch, out, 39, &words(&[0, 0, 3, 50]))?; // first and last font list
        // Ask for the whole desktop once: a server only sends what it considers
        // damaged, and a fresh client has drawn nothing yet.
        self.refresh(ch, out)
    }

    /// PDUTYPE2_REFRESH_RECT over the whole desktop. This is how a client that has
    /// just activated gets a full first picture instead of waiting for damage.
    fn refresh(&self, ch: &Channels, out: &mut impl Write) -> io::Result<()> {
        let mut body = vec![1u8]; // one area
        body.extend_from_slice(&[0; 3]); // pad
        body.extend_from_slice(&words(&[
            0,
            0,
            self.width.saturating_sub(1),
            self.height.saturating_sub(1),
        ]));
        self.send_data(ch, out, 33, &body)
    }

    fn send_control(
        &self,
        ch: &Channels,
        out: &mut impl Write,
        kind: u16,
        body: &[u8],
    ) -> io::Result<()> {
        let len =
            u16::try_from(body.len() + 6).map_err(|_| invalid("share control exceeds PDU size"))?;
        let mut packet = words(&[len, 0x10 | kind, ch.user]);
        packet.extend_from_slice(body);
        ch.send(out, ch.global, &packet)
    }

    fn send_data(
        &self,
        ch: &Channels,
        out: &mut impl Write,
        kind: u8,
        body: &[u8],
    ) -> io::Result<()> {
        let mut p = self.id.to_le_bytes().to_vec();
        p.extend_from_slice(&[0, 1]);
        p.extend_from_slice(&u16::try_from(body.len() + 4).unwrap_or(0).to_le_bytes());
        p.extend_from_slice(&[kind, 0, 0, 0]);
        p.extend_from_slice(body);
        self.send_control(ch, out, 7, &p)
    }

    pub(super) fn input(
        &self,
        ch: &Channels,
        out: &mut impl Write,
        kind: u16,
        flags: u16,
        a: u16,
        b: u16,
    ) -> io::Result<()> {
        self.send_data(ch, out, 28, &words(&[1, 0, 0, 0, kind, flags, a, b]))
    }

    pub(super) fn data<'a>(&self, bytes: &'a [u8]) -> io::Result<(u8, &'a [u8])> {
        let mut p = Packet(bytes);
        if p.le32()? != self.id {
            return Err(invalid("unexpected RDP share ID"));
        }
        p.take(4)?; // padding, stream ID, uncompressed length
        let kind = p.u8()?;
        let compression = p.u8()?;
        p.take(2)?;
        if compression != 0 {
            return Err(invalid("server used unnegotiated bulk compression"));
        }
        Ok((kind, p.0))
    }
}
