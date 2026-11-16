# Packaging

Every platform script takes a built binary and produces a distributable folder or
archive; nothing here builds Shellune itself or changes its behaviour.

| Platform | Script | Produce | Runs where |
| --- | --- | --- | --- |
| Windows x86_64 | `packaging/windows/make-portable.py` | `dist/Shellune-<version>-windows-x86_64-portable.zip` | Any host with Python 3; the binary must be a Windows build |
| Windows x86_64 | `packaging/windows/shellune.nsi` | `Shellune-<version>-setup.exe` via NSIS | Windows with NSIS 3 installed |
| macOS aarch64 | `packaging/macos/make-app-bundle.py` | `Shellune.app` | macOS; needs a Mach-O binary |
| macOS aarch64 | `packaging/macos/make-dmg.py` | `Shellune-<version>-macos-arm64.dmg` | macOS; run it on the signed bundle |
| macOS aarch64 | `tar` in `release.yml` | `Shellune-<version>-macos-arm64.app.tar.gz` (in-app update) | macOS; run it on the signed bundle |
| Linux x86_64 | `packaging/linux/make-deb.py` | `shellune_<version>_amd64.deb` | Linux; needs an ELF binary and the dpkg-dev tools |
| All | `packaging/updater-manifest.py` | `latest.json` for the in-app updater | Any host with Python 3; needs every file above signed |

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
- Linux: the deb is not code-signed (only the updater `.sig` described below, which
  dpkg does not read). Its `Depends` come from
  `dpkg-shlibdeps` over the built binary, so the build host needs the runtime
  libraries the binary links.

## In-app updates: signatures and latest.json

The application checks
`https://github.com/sakullla/shellune/releases/latest/download/latest.json` and
installs a file only when its minisign signature verifies against the public key
built into `crates/update` (`shellune_update::PUBLIC_KEY`) *and* the signature's
trusted comment names the manifest's version. This is separate from the operating
system code signing above.

| Manifest key | File | What the updater does |
| --- | --- | --- |
| `windows-x86_64` | `Shellune-<version>-setup.exe` | starts it with `/S /R` and quits (installed copy) |
| `windows-x86_64-portable` | `Shellune-<version>-windows-x86_64-portable.zip` | swaps `shellune.exe` in place (portable copy) |
| `darwin-aarch64` | `Shellune-<version>-macos-arm64.app.tar.gz` | replaces the `.app` (top-level `Shellune.app` in the archive) |
| `linux-x86_64` | `shellune_<version>_amd64.deb` | nothing; it opens the release page |

The release workflow signs each file right after it is built:

```sh
cargo run --release -p shellune-update --features release-tools --bin shellune-sign -- \
  sign dist/<file> --version <version>        # writes dist/<file>.sig
```

`shellune-sign` reads the secret key from `SHELLUNE_UPDATER_KEY` (the base64 of the
minisign secret key file) and its password from `SHELLUNE_UPDATER_KEY_PASSWORD`, and
exits with status 2 without either. Both are repository secrets; the release
workflow fails before building when they are missing, and the publish job checks
that `shellune-sign public-key` equals `PUBLIC_KEY` before anything is published.
The key pair lives outside the repository (`%USERPROFILE%\.shellune-release\`);
the secret key never enters it.

The publish job then writes the manifest and publishes it with every file (and the
`.sig` files) in one `gh release create`:

```sh
python3 packaging/updater-manifest.py --version <version> --dist dist \
  --notes-file docs/release/<version>-release-notes.md --output dist/latest.json
```

The script refuses (exit 1, no `latest.json` written) when any platform's file is
missing or empty, or its `.sig` is missing, empty, not a minisign signature, or
signed for another version. URLs point at
`https://github.com/sakullla/shellune/releases/download/v<version>/<file>`
(`--base-url` replaces the folder, for a local test server). Its tests:

```sh
python -m unittest discover -s packaging/tests -p "test_*.py"
```

A local dry run never uses the real key: make a throwaway pair with
`shellune-sign keygen --out <temporary folder>`, export its
`shellune-updater.key.base64` and `shellune-updater.password` as the two variables,
sign sample files named as above, run `updater-manifest.py`, and verify the result
with `shellune_update::verify_file` (or an `Updater` built with the test public
key); the built-in `PUBLIC_KEY` must refuse those signatures.

### Windows installer switches

`shellune.nsi` accepts `/S` (silent, NSIS built in), `/R` (start the installed
Shellune after a successful install; after a failed one, start the copy still in
place) and `/D=<folder>`. Before writing files it waits up to about 10 seconds for
the installed `shellune.exe` to exit (a running program cannot be opened for
writing); after that an interactive install asks to close it and a silent one runs
`taskkill /F /IM shellune.exe`, which ends every `shellune.exe` of the user, then
waits up to 5 more seconds and gives up without installing if the file is still in
use. The script is UTF-8 with a byte order mark for its Chinese strings.

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

## Native desktop backends (RDP, VNC)

The Windows binary links the RDP and VNC protocol libraries statically, so the
portable zip and the installer ship `shellune.exe` alone: there is no
`freerdp.dll` or `vncclient.dll` to place next to it, and a library installed
on the user's machine is never used. Build them before the release binary:

    python packaging/native/build_native.py --perl <perl with standard modules>
    cargo build --workspace --release

`target/native/dist/shellune-native.txt` records the pinned versions and the options
that were used. A build without it still compiles and starts, but
`crates/desktop` reports both protocols as unavailable - the packaging step must
not describe RDP or VNC support for a binary built that way.

The vendored sources and their licenses are described in `vendor/README.md`; a
distributable that includes the binary carries the same obligations as before
(GPL-3.0-only for Shellune, Apache-2.0 for FreeRDP and OpenSSL, GPL-2.0-or-later
for LibVNCServer). macOS and Linux builds of the protocol libraries are not wired
up yet, so those platforms must not claim desktop protocol support.
