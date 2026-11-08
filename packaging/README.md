# Packaging

Every platform script takes a built binary and produces a distributable folder or
archive; nothing here builds Shellune itself or changes its behaviour.

| Platform | Script | Produce | Runs where |
| --- | --- | --- | --- |
| Windows x86_64 | `packaging/windows/make-portable.py` | `dist/Shellune-<version>-windows-x86_64-portable.zip` | Any host with Python 3; the binary must be a Windows build |
| Windows x86_64 | `packaging/windows/shellune.nsi` | `Shellune-<version>-setup.exe` via NSIS | Windows with NSIS 3 installed |
| macOS aarch64 | `packaging/macos/make-app-bundle.py` | `Shellune.app` | macOS; needs a Mach-O binary |
| macOS aarch64 | `packaging/macos/make-dmg.py` | `Shellune-<version>-macos-arm64.dmg` | macOS; run it on the signed bundle |
| Linux x86_64 | `packaging/linux/make-deb.py` | `shellune_<version>_amd64.deb` | Linux; needs an ELF binary and the dpkg-dev tools |

Build the binary first, with the same profile the release uses:

```sh
cargo build --workspace --release
```

The path to the produced binary is `$CARGO_TARGET_DIR/release/shellune` (or
`shellune.exe` on Windows); `.cargo/config.toml` may move the target directory.

## Signing and notarisation state

- Windows: the portable zip contains an unsigned `shellune.exe`; SmartScreen shows
  the usual "unknown publisher" prompt. The NSIS script accepts
  `/DSIGNTOOL=...` and code-signs the installer only when `signtool` is provided.
- macOS: the release workflow signs `Shellune.app` with a **fixed self-signed
  certificate**, so every release carries the same signer and macOS keeps the
  keychain and privacy permissions granted to an earlier version. The certificate
  lives in the repository secrets `APPLE_CERTIFICATE` (base64 `.p12`),
  `APPLE_CERTIFICATE_PASSWORD` and `APPLE_SIGNING_IDENTITY` (the certificate's
  common name); a release without them fails instead of shipping an ad-hoc
  signature. The bundle is **not notarised**: on first launch Gatekeeper asks the
  user to allow it (right-click → Open, or System Settings → Privacy & Security).
  Replacing the certificate changes the signer, and users are asked again once.
- Linux: no signature at all; the deb has no signing step. Its `Depends` come from
  `dpkg-shlibdeps` over the built binary, so the build host needs the runtime
  libraries the binary links.

## What the scripts refuse to do

They do not invent a company name, a version or a license, and they do not
copy test helpers into a release artifact: `shellune-flood` exists only for the
interrupt tests, is hidden behind the `test-support` feature, and a `--release`
build does not contain it.

## Application icons

`assets/icons/` contains the generated source, PNG, multi-resolution Windows ICO
and macOS ICNS. The Windows build embeds icon resource 1, which GPUI uses for the
window and taskbar. The macOS and Linux packaging scripts include their icons.

`tool/generate_app_icons.py` adapts Rillight's configured-provider image workflow.
Its `PROMPT` defines a moon-blue/lavender crescent with a terminal-chevron cutout.
Generate with Python 3.11+ and Pillow, or rebuild offline from the saved source:

```sh
python tool/generate_app_icons.py --force
python tool/generate_app_icons.py --reuse-source --force
```
