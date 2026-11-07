# Releasing Shellune

## 1. Build

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features -- --include-ignored
cargo build --workspace --release
```

The release binary is `shellune` (`shellune.exe` on Windows) in the target
directory's `release` folder. `cargo build --release` does not include the
`shellune-flood` helper: it exists only for the interrupt tests and is hidden
behind the `test-support` feature.

## 2. Package

```sh
# Windows: portable zip (runs on any host with Python 3)
python packaging/windows/make-portable.py --binary <target>/release/shellune.exe

# Windows: installer (needs NSIS)
makensis packaging/windows/shellune.nsi

# macOS: app bundle (run on macOS)
python3 packaging/macos/make-app-bundle.py --binary <target>/release/shellune

# Linux: tarball with a desktop entry (run on Linux)
python3 packaging/linux/make-tarball.py --binary <target>/release/shellune
```

Everything lands in `dist/`, next to a `.sha256` file for the archive.

## 3. Sign

- Windows: nothing is signed by default. When a certificate is available, pass
  `/DSIGNTOOL=<path to signtool.exe>` to `makensis`; the installer is signed as the
  last step. Sign the portable `shellune.exe` before zipping it.
- macOS: `.github/workflows/release.yml` signs the bundle with the fixed self-signed
  certificate from the repository secrets (`APPLE_CERTIFICATE`,
  `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`), then verifies that the
  signature is not ad-hoc and that its authority is that identity. A release without
  the secrets fails. The bundle is not notarised. By hand:
  `codesign --force --options runtime --sign "<identity>" Shellune.app`.

## 4. Publish

Attach the archives from `dist/` and their `.sha256` files. State in the
announcement which platforms were actually run, not only built — see
`0.1.0-verification.md` for what that means for this version.

The window captures that back up the verification record are committed under
`docs/release/visual/` (see the README there for what each one shows); they are
part of the repository, not of the archives.

## 5. What a release must never claim

Shellune 0.1.0 has no split-screen terminals, no jump-host support, no port
forwarding and no resumable transfers. A release note that mentions any of them as
a feature is wrong.
