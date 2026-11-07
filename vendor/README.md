# Patched dependencies

## `xattr` 0.2.3

`gpui` 0.2.2 → `gpui_http_client` → `zed-async-tar` (default feature `xattr`) →
`xattr` 0.2.x. `xattr` 0.2.3 names `libc::ENOATTR` on Linux, but current `libc` 0.2
releases no longer export that constant for Linux, so the Linux build fails with
`E0425: cannot find value ENOATTR in crate libc`.

This copy is the released 0.2.3 crate with one change in `src/sys/mod.rs`: on Linux
and Android `ENOATTR` is `libc::ENODATA` (glibc and bionic define `ENOATTR` as
`ENODATA`); every other platform keeps `libc::ENOATTR`. The root `Cargo.toml` routes
the dependency here with `[patch.crates-io]`.

Remove this directory and the patch entry once `gpui` no longer pulls in `xattr` 0.2.
