//! Desktop activation, slow-path input and Share Control/Data PDUs.
use super::mcs::Channels;
use super::wire::{Packet, Security, invalid};
use std::io::{self, Write};
use zeroize::Zeroizing;

/// Enough for a 96x96 32-bpp cursor and its masks (MS-RDPBCGR 2.2.7.2.7).
pub(super) const MULTIFRAGMENT_LIMIT: u32 = 65536;
const FASTPATH_OUTPUT_SUPPORTED: u16 = 0x0001;
const NO_BITMAP_COMPRESSION_HDR: u16 = 0x0400;
const INPUT_FLAG_MOUSE_HWHEEL: u16 = 0x0100;
const NEGOTIATE_ORDER_SUPPORT: u16 = 0x0002;
const ZERO_BOUNDS_DELTAS_SUPPORT: u16 = 0x0008;

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
    pdu.extend_from_slice(&0x0000_000du32.to_le_bytes()); // keep full-window dragging enabled
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
    pub(super) horizontal_wheel: bool,
    pub(super) refresh_rects: bool,
    pub(super) large_pointers: bool,
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
            horizontal_wheel: false,
            refresh_rects: false,
            large_pointers: false,
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
        let mut horizontal_wheel = false;
        let mut refresh_rects = false;
        let mut multifragment = false;
        let mut large_pointers = false;
        for _ in 0..count {
            let kind = caps.le16()?;
            let length = caps
                .le16()?
                .checked_sub(4)
                .ok_or_else(|| invalid("short capability set"))?;
            let mut cap = Packet(caps.take(usize::from(length))?);
            if kind == 1 {
                cap.take(18)?;
                refresh_rects = cap.u8()? != 0;
            } else if kind == 2 {
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
            } else if kind == 13 {
                horizontal_wheel = cap.le16()? & INPUT_FLAG_MOUSE_HWHEEL != 0;
            } else if kind == 26 {
                multifragment = cap.le32()? >= 38055;
            } else if kind == 27 {
                large_pointers = cap.le16()? & 1 != 0;
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
            horizontal_wheel,
            refresh_rects,
            large_pointers: multifragment && large_pointers,
        })
    }

    pub(super) fn activate(&self, ch: &Channels, out: &mut impl Write) -> io::Result<()> {
        let mut caps = Capabilities {
            count: 0,
            bytes: Vec::new(),
        };
        // Windows sends bitmap updates on the fast path. Negotiate headerless
        // compression so bitmapLength starts at the actual codec stream.
        let extra_flags = FASTPATH_OUTPUT_SUPPORTED | NO_BITMAP_COMPRESSION_HDR;
        caps.add(1, &words(&[0, 0, 0x200, 0, 0, extra_flags, 0, 0, 0, 0]));
        // MS-RDPBCGR 2.2.7.1.2 requires both bitmapCompressionFlag and
        // multipleRectangleSupport, even when no drawing orders are supported.
        caps.add(
            2,
            &words(&[self.bpp, 1, 1, 1, self.width, self.height, 0, 1, 1, 0, 1, 0]),
        );
        let mut orders = vec![0; 84];
        let order_flags = NEGOTIATE_ORDER_SUPPORT | ZERO_BOUNDS_DELTAS_SUPPORT;
        orders[20..32].copy_from_slice(&words(&[1, 20, 0, 1, 0, order_flags]));
        caps.add(3, &orders);
        // Required client cache capabilities describe zero entries for this
        // bitmap-only renderer (MS-RDPBCGR 2.2.7.1).
        caps.add(4, &[0; 36]); // revision 1 bitmap cache
        // Both caches are supported, including New Pointer updates at the server's chosen depth.
        caps.add(
            8,
            &words(&[1, super::pointer::CACHE_SIZE, super::pointer::CACHE_SIZE]),
        );
        if self.large_pointers {
            caps.add(26, &MULTIFRAGMENT_LIMIT.to_le_bytes());
            caps.add(27, &1u16.to_le_bytes());
        }
        caps.add(9, &words(&[0, 0])); // share
        caps.add(12, &words(&[0, 0])); // no sound beeps
        let wheel = if self.horizontal_wheel {
            INPUT_FLAG_MOUSE_HWHEEL
        } else {
            0
        };
        let mut input = words(&[1 | wheel, 0]); // scan code and negotiated wheel input
        for v in [0x409u32, 4, 0, 12] {
            input.extend_from_slice(&v.to_le_bytes());
        }
        input.extend_from_slice(&[0; 64]);
        caps.add(13, &input);
        caps.add(14, &words(&[1, 0])); // font list
        caps.add(15, &0u32.to_le_bytes()); // no brush cache
        caps.add(16, &[0; 48]); // ten empty glyph caches, no fragments or glyph support
        caps.add(17, &[0; 8]); // no offscreen bitmap cache
        caps.add(20, &0u32.to_le_bytes()); // static virtual channels, no compression
        let mut confirm = self.id.to_le_bytes().to_vec();
        confirm.extend_from_slice(&words(&[1002, 8, (caps.bytes.len() + 4) as u16]));
        confirm.extend_from_slice(b"Shellune");
        confirm.extend_from_slice(&words(&[caps.count, 0]));
        confirm.extend_from_slice(&caps.bytes);
        // Connection finalization is one outbound flight. Font List enables
        // graphics on Windows, so send it in the same TLS write as Confirm Active.
        let mut batch = Vec::new();
        self.send_control(ch, &mut batch, 3, &confirm)?;
        self.send_data(ch, &mut batch, 31, &words(&[1, self.server]))?;
        self.send_data(ch, &mut batch, 20, &words(&[4, 0, 0, 0]))?; // cooperate
        self.send_data(ch, &mut batch, 20, &words(&[1, 0, 0, 0]))?; // request control
        self.send_data(ch, &mut batch, 39, &words(&[0, 0, 3, 50]))?; // first and last font list
        // Ask for the whole desktop once: a server only sends what it considers
        // damaged, and a fresh client has drawn nothing yet.
        if self.refresh_rects {
            self.refresh(ch, &mut batch)?;
        }
        out.write_all(&batch)?;
        out.flush()
    }

    /// PDUTYPE2_REFRESH_RECT over the whole desktop. This is how a client that has
    /// just activated gets a full first picture instead of waiting for damage.
    pub(super) fn refresh(&self, ch: &Channels, out: &mut impl Write) -> io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_active_advertises_the_required_bitmap_and_order_flags() -> io::Result<()> {
        let channels = Channels {
            dynamic: None,
            user: 1004,
            global: 1003,
            clipboard: None,
            drive: None,
        };
        let mut sent = Vec::new();
        Share::pending(1280, 720).activate(&channels, &mut sent)?;
        let data = super::super::wire::read_data(&mut sent.as_slice())?;
        let mut first = Packet(&data);
        first.expect(&[0x64])?;
        first.take(5)?; // MCS initiator, channel, priority
        let length = first.per_length()?;
        let header = control(&mut Packet(first.take(length)?))?;
        assert_eq!(header.kind, 3);
        let mut confirm = Packet(header.body);
        confirm.take(6)?; // share ID and originator
        let descriptor_len = confirm.le16()?;
        let caps_len = confirm.le16()?;
        confirm.take(usize::from(descriptor_len))?;
        let mut caps = Packet(confirm.take(usize::from(caps_len))?);
        confirm.finish()?;
        let count = caps.le16()?;
        caps.take(2)?;
        let mut general_flags = None;
        let mut bitmap_compression = None;
        let mut order_flags = None;
        let mut disabled = std::collections::BTreeMap::new();
        for _ in 0..count {
            let kind = caps.le16()?;
            let len = caps.le16()?;
            let mut cap = Packet(caps.take(usize::from(len) - 4)?);
            match kind {
                1 => {
                    cap.take(10)?;
                    general_flags = Some(cap.le16()?);
                }
                2 => {
                    cap.take(16)?;
                    bitmap_compression = Some(cap.le16()?);
                }
                3 => {
                    cap.take(30)?;
                    order_flags = Some(cap.le16()?);
                    assert_eq!(cap.take(32)?, &[0; 32]); // bitmap-only renderer
                }
                4 | 12 | 15 | 16 | 17 => {
                    assert!(cap.0.iter().all(|byte| *byte == 0));
                    disabled.insert(kind, len);
                }
                _ => {}
            }
        }
        caps.finish()?;
        assert_eq!(general_flags.map(|flags| flags & 0x0401), Some(0x0401));
        assert_eq!(bitmap_compression, Some(1));
        assert_eq!(order_flags.map(|flags| flags & 0x000a), Some(0x000a));
        assert_eq!(
            disabled,
            [(4, 40), (12, 8), (15, 8), (16, 52), (17, 12)].into()
        );
        Ok(())
    }
}
