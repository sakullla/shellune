//! Single-monitor display updates over the Display Control dynamic channel.
use super::mcs::Channels;
use super::wire::{Packet, invalid};
use std::io::{self, Write};
use std::time::{Duration, Instant};

/// Monitor scale hints shared by Client Core Data and Display Control.
pub(super) fn device_scale(percent: u32) -> u32 {
    match percent {
        0..120 => 100,
        120..160 => 140,
        _ => 180,
    }
}

pub(super) fn physical_mm(pixels: u32, percent: u32) -> u32 {
    (pixels * 2540 / (96 * percent)).clamp(10, 10000)
}

#[derive(Default)]
pub(super) struct Display {
    id: Option<u32>,
    max_area: u64,
    requested: Option<(u16, u16, u32)>,
    send_after: Option<Instant>,
    sent: Option<(u16, u16, u32)>,
}

impl Display {
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.send_after
            .filter(|_| self.id.is_some() && self.max_area >= 200 * 200)
    }

    pub(super) fn resize(&mut self, width: u16, height: u16, percent: u32) {
        if self.requested != Some((width, height, percent)) {
            self.requested = Some((width, height, percent));
            self.send_after = Some(Instant::now() + Duration::from_millis(200));
        }
    }

    pub(super) fn channel(
        &mut self,
        ch: &Channels,
        out: &mut impl Write,
        id: u16,
        bytes: &[u8],
    ) -> io::Result<()> {
        let mut chunk = Packet(bytes);
        let length = chunk.le32()? as usize;
        let flags = chunk.le32()?;
        // Version 1 DVC control messages and the single 20-byte display caps
        // fit in one static channel chunk.
        if flags & 3 != 3 || length != chunk.0.len() {
            return Ok(());
        }
        let header = chunk.u8()?;
        if header >> 4 == 5 {
            self.id = None;
            self.max_area = 0;
            self.sent = None;
            return ch.send_channel(out, id, &[], &[0x50, 0, 1, 0]);
        }
        let n = match header & 3 {
            0 => 1,
            1 => 2,
            2 => 4,
            _ => return Err(invalid("invalid dynamic channel ID")),
        };
        let encoded = chunk.take(n)?;
        let mut raw = [0; 4];
        raw[..n].copy_from_slice(encoded);
        let channel = u32::from_le_bytes(raw);
        match header >> 4 {
            1 => {
                let supported = chunk.0 == b"Microsoft::Windows::RDS::DisplayControl\0";
                let status = if supported { 0u32 } else { 0xc000_0001 };
                if supported {
                    self.id = Some(channel);
                    self.sent = None;
                    self.max_area = 0;
                }
                let mut reply = vec![header & 0xf3];
                reply.extend_from_slice(encoded);
                reply.extend_from_slice(&status.to_le_bytes());
                ch.send_channel(out, id, &[], &reply)
            }
            3 if self.id == Some(channel) => {
                if chunk.le32()? == 5 && chunk.le32()? == 20 {
                    let monitors = u64::from(chunk.le32()?);
                    let a = u64::from(chunk.le32()?);
                    let b = u64::from(chunk.le32()?);
                    self.max_area = monitors.saturating_mul(a).saturating_mul(b);
                    if self.requested.is_some() {
                        self.send_after.get_or_insert_with(Instant::now);
                    }
                }
                Ok(())
            }
            4 if self.id == Some(channel) => {
                self.id = None;
                self.max_area = 0;
                self.sent = None;
                let mut reply = vec![header & 0xf3];
                reply.extend_from_slice(encoded);
                ch.send_channel(out, id, &[], &reply)
            }
            _ => Ok(()),
        }
    }

    pub(super) fn poll(&mut self, ch: &Channels, out: &mut impl Write) -> io::Result<()> {
        let (Some(id), Some(channel), Some((w, h, percent))) =
            (ch.dynamic, self.id, self.requested)
        else {
            return Ok(());
        };
        let Some(deadline) = self.send_after else {
            return Ok(());
        };
        if self.max_area < 200 * 200 || Instant::now() < deadline {
            return Ok(());
        }
        self.send_after = None;
        let mut w = u32::from(w).clamp(200, 8192) & !1;
        let mut h = u32::from(h).clamp(200, 8192);
        if u64::from(w) * u64::from(h) > self.max_area {
            let scale = (self.max_area as f64 / f64::from(w * h)).sqrt();
            w = ((f64::from(w) * scale) as u32).max(200) & !1;
            h = ((f64::from(h) * scale) as u32).max(200);
            if u64::from(w * h) > self.max_area {
                return Ok(());
            }
        }
        let size = (w as u16, h as u16, percent);
        if self.sent == Some(size) {
            return Ok(());
        }
        // DYNVC_DATA with a four-byte channel ID, then DISPLAYCONTROL_MONITOR_LAYOUT_PDU.
        let mut message = vec![0x32];
        message.extend_from_slice(&channel.to_le_bytes());
        for value in [
            2,
            56,
            40,
            1,
            1,
            0,
            0,
            w,
            h,
            physical_mm(w, percent),
            physical_mm(h, percent),
            0,
            percent,
            device_scale(percent),
        ] {
            message.extend_from_slice(&value.to_le_bytes());
        }
        ch.send_channel(out, id, &[], &message)?;
        self.sent = Some(size);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "opt-in RDP/VNC test"]
    fn a_resize_waits_for_server_capabilities_and_only_sends_the_latest_layout() {
        let channels = Channels {
            user: 1004,
            global: 1003,
            clipboard: None,
            drive: None,
            dynamic: Some(1005),
        };
        let mut display = Display::default();
        let mut out = Vec::new();
        display.resize(1280, 720, 100);
        display.resize(1920, 1080, 200);
        display.send_after = Some(Instant::now());
        display.poll(&channels, &mut out).unwrap();
        assert!(out.is_empty());
        let chunk = |payload: &[u8]| {
            let mut buf = (payload.len() as u32).to_le_bytes().to_vec();
            buf.extend_from_slice(&3u32.to_le_bytes());
            buf.extend_from_slice(payload);
            buf
        };
        let mut open = vec![0x10, 7];
        open.extend_from_slice(b"Microsoft::Windows::RDS::DisplayControl\0");
        display
            .channel(&channels, &mut out, 1005, &chunk(&open))
            .unwrap();
        let mut caps = vec![0x30, 7];
        for value in [5u32, 20, 1, 4096, 4096] {
            caps.extend_from_slice(&value.to_le_bytes());
        }
        display
            .channel(&channels, &mut out, 1005, &chunk(&caps))
            .unwrap();
        out.clear();
        display.poll(&channels, &mut out).unwrap();
        let layout = &out[out.len() - 56..];
        assert_eq!(layout[28..32], 1920u32.to_le_bytes());
        assert_eq!(layout[32..36], 1080u32.to_le_bytes());
        assert_eq!(layout[48..52], 200u32.to_le_bytes());
        assert_eq!(layout[52..56], 180u32.to_le_bytes());
        out.clear();
        display.poll(&channels, &mut out).unwrap();
        assert!(out.is_empty());
        display.resize(1920, 1080, 100);
        display.send_after = Some(Instant::now());
        display.poll(&channels, &mut out).unwrap();
        assert_eq!(out[out.len() - 8..out.len() - 4], 100u32.to_le_bytes());
    }
}
