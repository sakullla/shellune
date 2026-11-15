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
