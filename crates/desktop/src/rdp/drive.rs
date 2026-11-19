//! The RDPDR drive channel: one mapped folder, served file by file.
//!
//! The server only ever names paths inside the device, but a name can still leave
//! the folder through "..", an absolute path or a link that resolves elsewhere. Every
//! open is re-checked here after resolution, so the channel can never reach a file the
//! user did not map.
use super::mcs::Channels;
use super::wire::{Packet, invalid};
use crate::{DriveProblem, DriveRefusal, Event, Notice};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

const PAKID_CORE_SERVER_ANNOUNCE: u16 = 0x496e;
const PAKID_CORE_CLIENTID_CONNECTION: u16 = 0x4343;
const PAKID_CORE_CLIENT_NAME: u16 = 0x434e;
const PAKID_CORE_SERVER_CAPABILITY: u16 = 0x5350;
const PAKID_CORE_CLIENT_CAPABILITY: u16 = 0x4350;
const PAKID_CORE_DEVICELIST_ANNOUNCE: u16 = 0x4441;
const PAKID_CORE_DEVICE_REPLY: u16 = 0x6472;
const PAKID_CORE_DEVICE_IOREQUEST: u16 = 0x4952;
const PAKID_CORE_DEVICE_IOCOMPLETION: u16 = 0x4943;

const RDPDR_DEVICE_FS: u32 = 8;
const DEVICE_ID: u32 = 1;

const IRP_MJ_CREATE: u32 = 0;
const IRP_MJ_CLOSE: u32 = 2;
const IRP_MJ_READ: u32 = 3;
const IRP_MJ_WRITE: u32 = 4;

const STATUS_SUCCESS: u32 = 0;
const STATUS_NOT_SUPPORTED: u32 = 0xc000_00bb;
const STATUS_ACCESS_DENIED: u32 = 0xc000_0022;
const STATUS_OBJECT_NAME_NOT_FOUND: u32 = 0xc000_0034;
const STATUS_INVALID_PARAMETER: u32 = 0xc000_000d;

/// How many files the server may hold open at once; beyond it a create fails
/// instead of growing without bound.
const MAX_OPEN: usize = 64;

pub(super) struct Drive {
    folder: Option<PathBuf>,
    emit: super::Emit,
    open: HashMap<u32, File>,
    next_file: u32,
}

impl Drive {
    pub(super) fn new(folder: Option<PathBuf>, emit: super::Emit) -> Drive {
        let folder = folder.map(|path| path.canonicalize().unwrap_or(path));
        Drive {
            folder,
            emit,
            open: HashMap::new(),
            next_file: 1,
        }
    }

    /// The server announces the channel; everything else follows that message.
    pub(super) fn on_data(
        &mut self,
        ch: &Channels,
        out: &mut impl Write,
        body: &[u8],
    ) -> io::Result<()> {
        let Some(id) = ch.drive else { return Ok(()) };
        let mut p = Packet(body);
        if p.le16()? != 0x4472 {
            return Err(invalid("unexpected RDPDR component"));
        }
        let _packet_id = p.le16()?;
        let kind = p.le16()?;
        match kind {
            PAKID_CORE_SERVER_ANNOUNCE => {
                let _version = p.le16()?;
                let _client = p.le32()?;
                self.connect(ch, out, id)
            }
            PAKID_CORE_SERVER_CAPABILITY => {
                let mut caps = Vec::new();
                for (capability, size) in [(1u16, 24usize), (5, 16)] {
                    caps.extend_from_slice(&capability.to_le_bytes());
                    caps.extend_from_slice(&u16::try_from(size + 8).unwrap_or(0).to_le_bytes());
                    caps.extend_from_slice(&2u32.to_le_bytes());
                    caps.extend_from_slice(&vec![0; size]);
                }
                let mut packet = Vec::new();
                packet.extend_from_slice(&2u16.to_le_bytes());
                packet.extend_from_slice(&0u16.to_le_bytes());
                packet.extend_from_slice(&caps);
                self.send(ch, out, id, PAKID_CORE_CLIENT_CAPABILITY, &[&packet])
            }
            PAKID_CORE_DEVICE_REPLY => {
                let _device = p.le32()?;
                let result = p.le32()?;
                if result != 0 {
                    (self.emit)(Event::Notice {
                        notice: Notice::DriveRefused(DriveRefusal::Declined(result)),
                    });
                }
                Ok(())
            }
            PAKID_CORE_DEVICE_IOREQUEST => {
                let device = p.le32()?;
                let file = p.le32()?;
                let completion = p.le32()?;
                let major = p.le32()?;
                let _minor = p.le32()?;
                let (status, data) = self.request(major, file, &mut p)?;
                let mut answer = Vec::with_capacity(data.len() + 12);
                answer.extend_from_slice(&device.to_le_bytes());
                answer.extend_from_slice(&completion.to_le_bytes());
                answer.extend_from_slice(&status.to_le_bytes());
                answer.extend_from_slice(&data);
                self.send(ch, out, id, PAKID_CORE_DEVICE_IOCOMPLETION, &[&answer])
            }
            _ => Ok(()),
        }
    }

    fn connect(&mut self, ch: &Channels, out: &mut impl Write, id: u16) -> io::Result<()> {
        let Some(folder) = self.folder.clone() else {
            return Ok(());
        };
        if let Err(problem) = folders(&folder) {
            (self.emit)(Event::Notice {
                notice: Notice::DriveUnavailable(problem),
            });
            return Ok(());
        }
        let client = 0x5348_4c4eu32; // "SHLN"
        self.send(
            ch,
            out,
            id,
            PAKID_CORE_CLIENTID_CONNECTION,
            &[
                &1u16.to_le_bytes()[..],
                &2u16.to_le_bytes()[..],
                &client.to_le_bytes()[..],
                &client.to_le_bytes()[..],
            ],
        )?;
        let mut name = Vec::new();
        name.extend_from_slice(&1u32.to_le_bytes());
        name.extend_from_slice(&0u32.to_le_bytes());
        let computer: Vec<u16> = "Shellune".encode_utf16().collect();
        name.extend_from_slice(
            &u32::try_from((computer.len() + 1) * 2)
                .unwrap_or(0)
                .to_le_bytes(),
        );
        for unit in &computer {
            name.extend_from_slice(&unit.to_le_bytes());
        }
        name.extend_from_slice(&0u16.to_le_bytes());
        self.send(ch, out, id, PAKID_CORE_CLIENT_NAME, &[&name])?;
        let mut list = Vec::new();
        list.extend_from_slice(&1u32.to_le_bytes());
        list.extend_from_slice(&RDPDR_DEVICE_FS.to_le_bytes());
        list.extend_from_slice(&DEVICE_ID.to_le_bytes());
        let mut drive_name = [0u8; 8];
        drive_name[..crate::DRIVE_NAME.len()].copy_from_slice(crate::DRIVE_NAME.as_bytes());
        list.extend_from_slice(&drive_name);
        self.send(ch, out, id, PAKID_CORE_DEVICELIST_ANNOUNCE, &[&list])
    }

    fn request(&mut self, major: u32, file: u32, p: &mut Packet<'_>) -> io::Result<(u32, Vec<u8>)> {
        let Some(root) = self.folder.clone() else {
            return Ok((STATUS_ACCESS_DENIED, Vec::new()));
        };
        match major {
            IRP_MJ_CREATE => {
                let _desired = p.le32()?;
                p.take(8)?;
                let _attributes = p.le32()?;
                let _shared = p.le32()?;
                let disposition = p.le32()?;
                let _options = p.le32()?;
                let length =
                    usize::try_from(p.le32()?).map_err(|_| invalid("RDPDR path too long"))?;
                let path = utf16(p.take(length)?)?;
                if self.open.len() >= MAX_OPEN {
                    return Ok((STATUS_ACCESS_DENIED, Vec::new()));
                }
                let resolved = match resolve(&root, &path) {
                    Ok(path) => path,
                    Err(status) => return Ok((status, Vec::new())),
                };
                let opened = match disposition {
                    1 => OpenOptions::new().read(true).open(&resolved),
                    0 | 2 => OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .open(&resolved),
                    3 => OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&resolved),
                    _ => return Ok((STATUS_INVALID_PARAMETER, Vec::new())),
                };
                match opened {
                    Ok(handle) => {
                        let id = self.next_file;
                        self.next_file = self.next_file.wrapping_add(1).max(1);
                        self.open.insert(id, handle);
                        let mut data = id.to_le_bytes().to_vec();
                        data.push(1); // FILE_OPENED
                        Ok((STATUS_SUCCESS, data))
                    }
                    Err(error) => Ok((status_of(&error), Vec::new())),
                }
            }
            IRP_MJ_READ => {
                let length = usize::try_from(p.le32()?).unwrap_or(0);
                if length > crate::MAX_FRAME_BYTES {
                    return Ok((STATUS_INVALID_PARAMETER, Vec::new()));
                }
                let offset = u64::from_le_bytes(p.take(8)?.try_into().unwrap());
                let Some(handle) = self.open.get_mut(&file) else {
                    return Ok((STATUS_INVALID_PARAMETER, Vec::new()));
                };
                let mut buffer = vec![0; length];
                handle.seek(SeekFrom::Start(offset))?;
                let read = handle.read(&mut buffer)?;
                buffer.truncate(read);
                Ok((STATUS_SUCCESS, buffer))
            }
            IRP_MJ_WRITE => {
                let length = usize::try_from(p.le32()?).unwrap_or(0);
                let offset = u64::from_le_bytes(p.take(8)?.try_into().unwrap());
                let data = p.take(length)?;
                let Some(handle) = self.open.get_mut(&file) else {
                    return Ok((STATUS_INVALID_PARAMETER, Vec::new()));
                };
                handle.seek(SeekFrom::Start(offset))?;
                handle.write_all(data)?;
                Ok((
                    STATUS_SUCCESS,
                    u32::try_from(data.len())
                        .unwrap_or(0)
                        .to_le_bytes()
                        .to_vec(),
                ))
            }
            IRP_MJ_CLOSE => {
                self.open.remove(&file);
                Ok((STATUS_SUCCESS, Vec::new()))
            }
            _ => Ok((STATUS_NOT_SUPPORTED, Vec::new())),
        }
    }

    fn send(
        &self,
        ch: &Channels,
        out: &mut impl Write,
        channel: u16,
        kind: u16,
        parts: &[&[u8]],
    ) -> io::Result<()> {
        let mut message = Vec::new();
        message.extend_from_slice(&0x4472u16.to_le_bytes());
        message.extend_from_slice(&kind.to_le_bytes());
        for part in parts {
            message.extend_from_slice(part);
        }
        ch.send(out, channel, &message)
    }

    pub(super) fn close(&mut self, _ch: &Channels, _out: &mut impl Write) -> io::Result<()> {
        self.open.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        let folder = std::env::temp_dir().join("shellune-rdpdr-resolve");
        let _ = std::fs::create_dir_all(folder.join("sub"));
        std::fs::write(folder.join("sub").join("file.txt"), b"x").unwrap();
        folder.canonicalize().unwrap()
    }

    #[test]
    fn a_device_path_stays_inside_the_mapped_folder() {
        let root = root();
        assert_eq!(
            resolve(&root, "sub\\file.txt").unwrap(),
            root.join("sub").join("file.txt")
        );
        // A leading separator is a path inside the device, not the host root.
        assert_eq!(
            resolve(&root, "\\sub\\file.txt").unwrap(),
            root.join("sub").join("file.txt")
        );
    }

    #[test]
    fn escaping_the_folder_is_refused() {
        let root = root();
        assert_eq!(resolve(&root, "..\\outside.txt"), Err(STATUS_ACCESS_DENIED));
        assert_eq!(resolve(&root, "sub\\..\\..\\x"), Err(STATUS_ACCESS_DENIED));
        // A DOS drive prefix is not a name inside the folder.
        assert_eq!(resolve(&root, "C:\\windows"), Err(STATUS_ACCESS_DENIED));
    }
}

/// The MCS data inside one TPKT frame.
fn mcs_payload(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.len() < 11 || bytes[0] != 3 {
        return None;
    }
    let total = usize::from(u16::from_be_bytes([bytes[2], bytes[3]]));
    if total < 11 || total > bytes.len() {
        return None;
    }
    let mcs = &bytes[7..total];
    if mcs.first() != Some(&0x64) || mcs.get(5) != Some(&0x70) {
        return None;
    }
    let mut index = 6usize;
    let mut length = usize::from(*mcs.get(index)?);
    index += 1;
    if length & 0x80 != 0 {
        length = ((length & 0x7f) << 8) | usize::from(*mcs.get(index)?);
        index += 1;
    }
    mcs.get(index..index + length)
}

/// One canned IRP for the test hook below.
pub(crate) struct ProbeRequest<'a> {
    pub major: u32,
    pub minor: u32,
    pub on_file: bool,
    pub body: &'a [u8],
}

/// Test hook: runs the drive channel over canned IRPs, in order, and returns the
/// NTSTATUS each was answered with. This is the code a session runs, not a copy.
pub(crate) fn probe_requests(folder: &std::path::Path, requests: &[ProbeRequest<'_>]) -> Vec<u32> {
    let emit: super::Emit = std::sync::Arc::new(|_event| {});
    let mut drive = Drive::new(Some(folder.to_path_buf()), emit);
    let channels = Channels {
        user: 1005,
        global: 1003,
        clipboard: None,
        drive: Some(1006),
    };
    let mut out = Vec::new();
    let mut file = 0u32;
    let mut statuses = Vec::new();
    for request in requests {
        let mut message = vec![0x72, 0x44, 0, 0];
        message.extend_from_slice(&0x4952u16.to_le_bytes()); // PAKID_CORE_DEVICE_IOREQUEST
        message.extend_from_slice(&DEVICE_ID.to_le_bytes());
        message.extend_from_slice(&file.to_le_bytes());
        message.extend_from_slice(&1u32.to_le_bytes()); // completion id
        message.extend_from_slice(&request.major.to_le_bytes());
        message.extend_from_slice(&request.minor.to_le_bytes());
        message.extend_from_slice(request.body);
        out.clear();
        let _ = drive.on_data(&channels, &mut out, &message);
        // The answer is wrapped in TPKT, X.224 and MCS; its body is the RDPDR
        // DEVICE_IOCOMPLETION: component, kind, device, completion, status, data.
        let answer = mcs_payload(&out).unwrap_or(&[]);
        let status = if answer.len() >= 16 {
            u32::from_le_bytes([answer[12], answer[13], answer[14], answer[15]])
        } else {
            u32::MAX
        };
        let data = answer.get(16..).unwrap_or(&[]);
        if data.len() >= 5 {
            file = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        }
        statuses.push(status);
        if !request.on_file {
            // A request that does not name the open file still owns the handle it
            // was answered with; keep it for the next one.
        }
    }
    statuses
}

fn folders(folder: &Path) -> Result<(), DriveProblem> {
    if !folder.is_absolute() {
        return Err(DriveProblem::NotAbsolute);
    }
    let listing = fs::read_dir(folder).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => DriveProblem::Missing,
        io::ErrorKind::PermissionDenied => DriveProblem::NoAccess,
        _ => DriveProblem::Other(-1),
    })?;
    drop(listing);
    Ok(())
}

/// Resolves a device path inside the mapped folder. "..", absolute names and any
/// name that canonicalises outside the folder are refused.
fn resolve(root: &Path, device: &str) -> Result<PathBuf, u32> {
    let relative = device.replace('\\', "/");
    let mut target = PathBuf::from(root);
    for part in relative.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if Path::new(part)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
            || part.contains(':')
            || device_name(part)
        {
            return Err(STATUS_ACCESS_DENIED);
        }
        target.push(part);
    }
    match target.canonicalize() {
        Ok(resolved) if resolved.starts_with(root) => Ok(resolved),
        Ok(_) => Err(STATUS_ACCESS_DENIED),
        // A name that does not exist yet can still be created: the parent is what
        // has to be inside the folder, and it is canonicalised so a link cannot
        // stand in for it.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = target.parent().ok_or(STATUS_ACCESS_DENIED)?;
            let name = target.file_name().ok_or(STATUS_ACCESS_DENIED)?;
            let parent = parent.canonicalize().map_err(|error| status_of(&error))?;
            if !parent.starts_with(root) {
                return Err(STATUS_ACCESS_DENIED);
            }
            Ok(parent.join(name))
        }
        Err(error) => Err(status_of(&error)),
    }
}

/// Whether a name names a DOS device rather than a file: Windows resolves these
/// before the directory, so a mapped folder must never pass one through.
fn device_name(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or(part).to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn status_of(error: &io::Error) -> u32 {
    match error.kind() {
        io::ErrorKind::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
        _ => STATUS_ACCESS_DENIED,
    }
}

fn utf16(bytes: &[u8]) -> io::Result<String> {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let unit = u16::from_le_bytes([pair[0], pair[1]]);
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    String::from_utf16(&units).map_err(|_| invalid("RDPDR path is not valid UTF-16"))
}
