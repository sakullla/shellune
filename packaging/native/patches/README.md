# FreeRDP 3.33.1: classic RDP MAC negotiation

`freerdp-salted-mac-negotiation.patch` fixes the client connection sequence:

- Reset `do_secure_checksum` before Client Info. Server capabilities have not
  arrived yet, so the configured `SaltedChecksum=TRUE` default is not negotiated.
- After Demand Active, assign the negotiated setting before Confirm Active.
  Both values must be assigned, including during reactivation.

[MS-RDPBCGR 5.3.6.1.1](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/f390586a-3fdd-4a96-bbe0-a31575e92056)
specifies capability negotiation for salted MAC. xrdp 0.10.1-3.1+deb13u2
supports only ordinary MAC and now verifies incoming slow-path signatures.
The premature salted Client Info fails that check. See the
[upstream xrdp report](https://github.com/neutrinolabs/xrdp/issues/3808).

This patch preserves encryption and signature verification, retains salted MAC
for capable peers, and does not change TLS/NLA selection or certificate policy.
The native builder checks the complete source file before/after patching and
includes the patch SHA-256 in the build stamp and manifest. Re-running it rebuilds
FreeRDP when needed; Cargo then relinks via the changed manifest.

```powershell
python packaging/native/build_native.py
python packaging/native/test_rdp_mac.py
cargo test -p shellune-desktop
```

The C regression links the actual static library and captures its encrypted
Client Info/Confirm Active packets. It tests initial/reconnect state and both
negotiated values across reactivation. It deliberately stops the transport after
Confirm Active, so it does not claim to test a complete salted-capable server.
The Python runner also checks fresh/repeated patch application and rejects edited
dependency sources. Real xrdp classic and TLS results are recorded in
`docs/release/2026-11-16-protocol-hardening.md`.

# FreeRDP 3.33.1: drive confinement

`freerdp-drive-confinement.patch` keeps an RDP server inside the folder that
Shellune maps as a drive (`channels/drive/client/drive_file.c`).

Upstream only rejects `..` in the path text. `CreateFileW` and `FindFirstFileW`
still follow symbolic links, junctions and mount points. A link inside the mapped
folder would therefore give the server whatever the link points at.

With the patch, these operations are resolved before they run:

- open and create
- delete on close
- the initial directory listing
- the rename target

The deepest part of the path that exists is opened without data access. Its final
path must lie within the final path of the base. Windows gets the final path from
`GetFinalPathNameByHandleW`; other systems use `realpath`. Anything else is refused
with `ERROR_ACCESS_DENIED`, which the server sees as `STATUS_ACCESS_DENIED`.

The patch also fixes three related cases:

- **Handle check.** On Windows, an opened handle is checked again. This also
  catches a link swapped in between the check and the open.
- **Delete on close.** A create that was refused, or that failed, no longer
  deletes the path it named on close.
- **Refused paths.** A path rejected before any call sets an error (for example
  one containing `..`) is now answered with access denied. Before, it was
  answered with the thread's stale last error, often `STATUS_SUCCESS`.

# FreeRDP 3.33.1: rdpdr device reply

`freerdp-rdpdr-device-reply.patch` adds two optional callbacks to
`RdpdrClientContext`:

- `RdpdrServerAnnounce`, called when the server opens device redirection.
- `RdpdrDeviceReply`, called with the server's result code for each announced
  device ([MS-RDPEFS] 2.2.2.1).

The shim hooks both callbacks when FreeRDP initialises the channel. It reports
`SD_NOTICE_DRIVE_REFUSED` in two cases:

- The server declines the mapped drive. The notice detail is the NTSTATUS.
- The server never opens device redirection within the logon grace period. The
  notice detail is 0.

Both patches follow the MAC patch's model. The builder checks every file's SHA-256
before and after patching, and puts each patch's SHA-256 in the build pin and the
manifest. `python packaging/native/test_patches.py` checks three things for every
patch:

- It applies to a fresh copy of the source.
- Applying it again is harmless.
- An edited source is refused.

A work tree that still holds an earlier revision of one of these patches is
refused. Restore the original file (or unpack FreeRDP again) and rebuild.
