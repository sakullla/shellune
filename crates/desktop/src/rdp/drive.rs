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
const IRP_MJ_SET_INFORMATION: u32 = 6;
const IRP_MJ_DIRECTORY_CONTROL: u32 = 12;
const IRP_MN_QUERY_DIRECTORY: u32 = 1;

/// Create options this channel honours.
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
const FILE_DELETE_ON_CLOSE: u32 = 0x0000_1000;

/// The information class of a rename.
const FILE_RENAME_INFORMATION: u32 = 10;

const STATUS_SUCCESS: u32 = 0;
const STATUS_NOT_SUPPORTED: u32 = 0xc000_00bb;
const STATUS_ACCESS_DENIED: u32 = 0xc000_0022;
const STATUS_OBJECT_NAME_NOT_FOUND: u32 = 0xc000_0034;
const STATUS_INVALID_PARAMETER: u32 = 0xc000_000d;
const STATUS_NO_MORE_FILES: u32 = 0x8000_001a;
const STATUS_NOT_A_DIRECTORY: u32 = 0xc000_010e;

/// How many files the server may hold open at once; beyond it a create fails
/// instead of growing without bound.
const MAX_OPEN: usize = 64;

/// One file the server has open, with what it takes to close it properly.
struct Open {
    handle: File,
    path: PathBuf,
    directory: bool,
    delete_on_close: bool,
}

pub(super) struct Drive {
    folder: Option<PathBuf>,
    emit: super::Emit,
    open: HashMap<u32, Open>,
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
                let minor = p.le32()?;
                let (status, data) = self.request(major, minor, file, &mut p)?;
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

    fn request(
        &mut self,
        major: u32,
        minor: u32,
        file: u32,
        p: &mut Packet<'_>,
    ) -> io::Result<(u32, Vec<u8>)> {
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
                let options = p.le32()?;
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
                let directory = options & FILE_DIRECTORY_FILE != 0
                    || resolved
                        .metadata()
                        .map(|meta| meta.is_dir())
                        .unwrap_or(false);
                let delete_on_close = options & FILE_DELETE_ON_CLOSE != 0;
                let opened = if directory {
                    open_directory(&resolved, disposition)
                } else {
                    open_file(&resolved, disposition)
                };
                match opened {
                    Ok(handle) => {
                        let id = self.next_file;
                        self.next_file = self.next_file.wrapping_add(1).max(1);
                        self.open.insert(
                            id,
                            Open {
                                handle,
                                path: resolved,
                                directory,
                                delete_on_close,
                            },
                        );
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
                let Some(open) = self.open.get_mut(&file) else {
                    return Ok((STATUS_INVALID_PARAMETER, Vec::new()));
                };
                if open.directory {
                    return Ok((STATUS_INVALID_PARAMETER, Vec::new()));
                }
                let mut buffer = vec![0; length];
                open.handle.seek(SeekFrom::Start(offset))?;
                let read = open.handle.read(&mut buffer)?;
                buffer.truncate(read);
                Ok((STATUS_SUCCESS, buffer))
            }
            IRP_MJ_WRITE => {
                let length = usize::try_from(p.le32()?).unwrap_or(0);
                let offset = u64::from_le_bytes(p.take(8)?.try_into().unwrap());
                let data = p.take(length)?;
                let Some(open) = self.open.get_mut(&file) else {
                    return Ok((STATUS_INVALID_PARAMETER, Vec::new()));
                };
                if open.directory {
                    return Ok((STATUS_INVALID_PARAMETER, Vec::new()));
                }
                open.handle.seek(SeekFrom::Start(offset))?;
                open.handle.write_all(data)?;
                Ok((
                    STATUS_SUCCESS,
                    u32::try_from(data.len())
                        .unwrap_or(0)
                        .to_le_bytes()
                        .to_vec(),
                ))
            }
            IRP_MJ_CLOSE => {
                if let Some(open) = self.open.remove(&file) {
                    if open.delete_on_close {
                        let Open { handle, path, .. } = open;
                        drop(handle);
                        let _ = if path.is_dir() {
                            fs::remove_dir_all(&path)
                        } else {
                            fs::remove_file(&path)
                        };
                    }
                }
                Ok((STATUS_SUCCESS, Vec::new()))
            }
            IRP_MJ_DIRECTORY_CONTROL if minor == IRP_MN_QUERY_DIRECTORY => {
                self.query_directory(&root, p)
            }
            IRP_MJ_SET_INFORMATION => self.set_information(&root, file, p),
            _ => Ok((STATUS_NOT_SUPPORTED, Vec::new())),
        }
    }

    /// Lists a directory the server names. The pattern is the last part of the
    /// path, and the directory it names is resolved like any other: a link that
    /// leads out of the folder is refused, not listed.
    fn query_directory(&mut self, root: &Path, p: &mut Packet<'_>) -> io::Result<(u32, Vec<u8>)> {
        let _class = p.le32()?;
        let initial = p.u8()?;
        let length = usize::try_from(p.le32()?).map_err(|_| invalid("RDPDR path too long"))?;
        // The rest of the fixed part is padding the protocol defines.
        p.take(23)?;
        let path = utf16(p.take(length)?)?;
        if initial == 0 {
            return Ok((STATUS_NO_MORE_FILES, Vec::new()));
        }
        let (directory, pattern) = split_pattern(&path);
        let directory = match resolve(root, &directory) {
            Ok(path) => path,
            Err(status) => return Ok((status, Vec::new())),
        };
        if !directory.is_dir() {
            return Ok((STATUS_NOT_A_DIRECTORY, Vec::new()));
        }
        let mut data = Vec::new();
        let mut previous = None;
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !matches_pattern(&pattern, &name) {
                continue;
            }
            let meta = entry.metadata()?;
            match push_entry(&mut data, previous, &name, meta.is_dir(), meta.len()) {
                Some(begin) => previous = Some(begin),
                None => break,
            }
        }
        Ok((STATUS_SUCCESS, data))
    }

    /// Renames one open file inside the folder. The new name is resolved like an
    /// open, so a rename can never move a file out through a link.
    fn set_information(
        &mut self,
        root: &Path,
        file: u32,
        p: &mut Packet<'_>,
    ) -> io::Result<(u32, Vec<u8>)> {
        let class = p.le32()?;
        let length = usize::try_from(p.le32()?).map_err(|_| invalid("RDPDR rename too long"))?;
        p.take(24)?;
        if class != FILE_RENAME_INFORMATION {
            return Ok((STATUS_NOT_SUPPORTED, Vec::new()));
        }
        let mut info = Packet(p.take(length)?);
        let _replace_if_exists = info.u8()?;
        let _root_directory = info.u8()?;
        let name_length =
            usize::try_from(info.le32()?).map_err(|_| invalid("RDPDR name too long"))?;
        let name = utf16(info.take(name_length)?)?;
        let Some(open) = self.open.get(&file) else {
            return Ok((STATUS_INVALID_PARAMETER, Vec::new()));
        };
        let target = match resolve(root, &name) {
            Ok(path) => path,
            Err(status) => return Ok((status, Vec::new())),
        };
        match fs::rename(&open.path, &target) {
            Ok(()) => Ok((STATUS_SUCCESS, Vec::new())),
            Err(error) => Ok((status_of(&error), Vec::new())),
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

    /// Ends the channel: every handle is closed, and a file opened to be deleted on
    /// close goes now rather than staying behind.
    pub(super) fn close(&mut self, _ch: &Channels, _out: &mut impl Write) -> io::Result<()> {
        for (_, open) in self.open.drain() {
            if open.delete_on_close {
                let Open { handle, path, .. } = open;
                drop(handle);
                let _ = if path.is_dir() {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_file(&path)
                };
            }
        }
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
        let _ = request.on_file;
    }
    // Releasing the device closes what is still open, which is also what deletes a
    // file that was opened to be deleted on close.
    let _ = drive.close(&channels, &mut out);
    statuses
}

/// Opens a file the way the disposition asks: 1 opens an existing one, 2 and 0
/// open or create, 3 creates a new one.
fn open_file(path: &Path, disposition: u32) -> io::Result<File> {
    match disposition {
        1 => OpenOptions::new().read(true).open(path),
        0 | 2 => OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path),
        3 => OpenOptions::new().write(true).create_new(true).open(path),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported RDPDR disposition",
        )),
    }
}

/// Opens a directory, creating it when the disposition asks for a new one. A
/// directory needs a flag on Windows that a plain open never passes.
fn open_directory(path: &Path, disposition: u32) -> io::Result<File> {
    if matches!(disposition, 2 | 3) && !path.exists() {
        fs::create_dir(path)?;
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0200_0000); // FILE_FLAG_BACKUP_SEMANTICS
    }
    options.open(path)
}

/// Splits a device path into the directory and the last name part, which is a
/// pattern in a listing request.
fn split_pattern(path: &str) -> (String, String) {
    let normalized = path.replace('/', "\\");
    match normalized.rfind('\\') {
        Some(index) => (
            normalized[..index].to_owned(),
            normalized[index + 1..].to_owned(),
        ),
        None => (normalized, "*".to_owned()),
    }
}

/// Matches the name patterns an RDP client sends: `*`, `*.*` and an exact name.
/// Anything more elaborate is left out rather than guessed at.
fn matches_pattern(pattern: &str, name: &str) -> bool {
    if pattern.is_empty() || pattern == "*" || pattern == "*.*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    pattern.eq_ignore_ascii_case(name)
}

/// Appends one FILE_BOTH_DIR_INFORMATION entry, linking it to the one before.
/// Returns the offset of the new entry, or `None` when the response is full.
fn push_entry(
    data: &mut Vec<u8>,
    previous: Option<usize>,
    name: &str,
    directory: bool,
    size: u64,
) -> Option<usize> {
    let units: Vec<u16> = name.encode_utf16().collect();
    // A listing that would not fit is cut short rather than sent oversized.
    if data.len() + 94 + units.len() * 2 > 16 * 1024 {
        return None;
    }
    let begin = data.len();
    if let Some(previous) = previous {
        let offset = u32::try_from(begin - previous).unwrap_or(0);
        data[previous..previous + 4].copy_from_slice(&offset.to_le_bytes());
    }
    data.resize(begin + 94 + units.len() * 2, 0);
    let entry = &mut data[begin..];
    entry[0..4].copy_from_slice(&0u32.to_le_bytes()); // NextEntryOffset, set later
    entry[4..8].copy_from_slice(&0u32.to_le_bytes()); // FileIndex
    entry[40..48].copy_from_slice(&if directory { 0 } else { size }.to_le_bytes());
    entry[48..56].copy_from_slice(&size.to_le_bytes());
    let attributes: u32 = if directory { 0x10 } else { 0x80 };
    entry[56..60].copy_from_slice(&attributes.to_le_bytes());
    let bytes = u32::try_from(units.len() * 2).unwrap_or(0);
    entry[60..64].copy_from_slice(&bytes.to_le_bytes());
    entry[64..68].copy_from_slice(&0u32.to_le_bytes()); // EaSize
    entry[68] = 0; // ShortNameLength
    entry[69] = 0; // Reserved
    for (slot, unit) in entry[94..].chunks_exact_mut(2).zip(units.iter()) {
        slot.copy_from_slice(&unit.to_le_bytes());
    }
    Some(begin)
}

/// Test hook: runs the drive channel's device exchange with a server that replies
/// with `result` (0 accepts the device), and returns the events it reported.
pub(crate) fn probe_drive_reply(folder: &str, result: u32) -> Vec<Event> {
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&events);
    let emit: super::Emit = std::sync::Arc::new(move |event| {
        if let Ok(mut slot) = sink.lock() {
            slot.push(event);
        }
    });
    let mut drive = Drive::new(Some(std::path::PathBuf::from(folder)), emit);
    let channels = Channels {
        user: 1005,
        global: 1003,
        clipboard: None,
        drive: Some(1006),
    };
    let mut out = Vec::new();
    // SERVER_ANNOUNCE: component, packet id, kind, version, client id.
    let mut announce = vec![0x72, 0x44, 0, 0];
    announce.extend_from_slice(&PAKID_CORE_SERVER_ANNOUNCE.to_le_bytes());
    announce.extend_from_slice(&1u16.to_le_bytes());
    announce.extend_from_slice(&2u32.to_le_bytes());
    let _ = drive.on_data(&channels, &mut out, &announce);
    // DEVICE_REPLY: component, packet id, kind, device id, result.
    let mut reply = vec![0x72, 0x44, 0, 0];
    reply.extend_from_slice(&PAKID_CORE_DEVICE_REPLY.to_le_bytes());
    reply.extend_from_slice(&DEVICE_ID.to_le_bytes());
    reply.extend_from_slice(&result.to_le_bytes());
    let _ = drive.on_data(&channels, &mut out, &reply);
    let events = events.lock().map(|slot| slot.clone()).unwrap_or_default();
    events
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
