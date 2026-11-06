# Packaging

Every platform script takes a built binary and produces a distributable folder or
archive; nothing here builds Shellune itself or changes its behaviour.

| Platform | Script | Produce | Runs where |
| --- | --- | --- | --- |
| Windows x86_64 | `packaging/windows/make-portable.py` | `dist/Shellune-<version>-windows-x86_64-portable.zip` | Any host with Python 3; the binary must be a Windows build |
| Windows x86_64 | `packaging/windows/shellune.nsi` | `Shellune-<version>-setup.exe` via NSIS | Windows with NSIS 3 installed |
| macOS aarch64 | `packaging/macos/make-app-bundle.py` | `Shellune.app` (+ optional `.zip`) | macOS; needs a Mach-O binary |
| Linux x86_64 | `packaging/linux/make-tarball.py` | `Shellune-<version>-linux-x86_64.tar.gz` | Linux; needs an ELF binary |

Build the binary first, with the same profile the release uses:

```sh
cargo build --workspace --release
```

The path to the produced binary is `$CARGO_TARGET_DIR/release/shellune` (or
`shellune.exe` on Windows); `.cargo/config.toml` may move the target directory.

## Signing and notarisation state

**Nothing in this repository is signed.** No code-signing certificate, no Apple
Developer ID and no notarisation credentials are available to this project, so the
artifacts are unsigned unless the person packaging them supplies a certificate:

- Windows: the portable zip contains an unsigned `shellune.exe`; SmartScreen shows
  the usual "unknown publisher" prompt. The NSIS script accepts
  `/DSIGNTOOL=...` and code-signs the installer only when `signtool` is provided.
- macOS: the bundle is unsigned and unnotarised. Gatekeeper blocks it until the user
  allows it, or until a Developer ID certificate is used (`codesign` +
  `notarytool`, wired in `.github/workflows/release.yml` but skipped unless the
  signing secrets exist).
- Linux: no signature at all; a tarball has no signing step.

## What the scripts refuse to do

They do not invent an icon, a company name, a version or a license, and they do not
copy test helpers into a release artifact: `shellune-flood` exists only for the
interrupt tests, is hidden behind the `test-support` feature, and a `--release`
build does not contain it.
