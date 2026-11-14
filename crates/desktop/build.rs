//! Compiles the C shim and links it to the vendored protocol libraries.
//!
//! Two shapes are possible, and the crate works in both:
//!
//! * **native**: `packaging/native/build_native.py` has produced
//!   `target/native/dist/shellune-native.txt` describing the include directories,
//!   the static libraries and the system libraries they need. The shim is compiled
//!   with `SD_WITH_RDP` and `SD_WITH_VNC`.
//! * **stub**: that file does not exist. The same symbols are compiled from
//!   `shim/sd_stub.c`, and `sd_backend_available` reports that the protocol
//!   is not built. Nothing claims a capability the binary does not have.
//!
//! Set `SHELLUNE_DESKTOP_NATIVE` to point at another installed prefix.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=SHELLUNE_DESKTOP_NATIVE");
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let dist = env::var("SHELLUNE_DESKTOP_NATIVE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            manifest_dir
                .join("..")
                .join("..")
                .join("target/native/dist")
        });
    let config_path = dist.join("shellune-native.txt");
    println!("cargo:rerun-if-changed={}", config_path.display());

    let mut build = cc::Build::new();
    for file in ["sd_shim.c", "sd_rdp.c", "sd_vnc.c", "sd_stub.c"] {
        let path = manifest_dir.join("shim").join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        build.file(path);
    }
    build.include(manifest_dir.join("shim"));
    build.define("_CRT_SECURE_NO_WARNINGS", None);
    // The vendored headers are not warning-clean under MSVC /W4, and they are not
    // ours to fix.
    build.warnings(false);

    match fs::read_to_string(&config_path) {
        Ok(text) => configure_native(&text, &mut build),
        Err(_) => {
            println!(
                "cargo:warning=native desktop backends not built; run \
                 packaging/native/build_native.py to link the vendored FreeRDP and \
                 LibVNCClient (looked for {})",
                config_path.display()
            );
        }
    }

    build.compile("shellune_desktop_shim");
}

fn configure_native(text: &str, build: &mut cc::Build) {
    let mut protocols = 0;
    // FreeRDP and WinPR mark their API as dllimport unless the consumer says it is
    // building them. This build links the static archives, so the same defines their
    // own static targets use keep the declarations plain instead of importing.
    build.define("FREERDP_EXPORTS", None);
    build.define("WINPR_EXPORTS", None);
    // Both libraries are built by CMake, which defines WIN32 and sets the API level.
    // LibVNCClient's headers pick rfbBool and rfbSocket by that macro, so a consumer
    // that leaves it undefined reads a different rfbClient layout than the library
    // wrote to.
    build.define("WIN32", None);
    build.define("_WIN32_WINNT", "0x0600");
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "include" => {
                build.include(value);
            }
            "lib_dir" => {
                println!("cargo:rustc-link-search=native={value}");
            }
            "lib" => {
                println!("cargo:rustc-link-lib=static={value}");
            }
            "syslib" => {
                println!("cargo:rustc-link-lib={value}");
            }
            "protocol" => match value {
                "rdp" => {
                    build.define("SD_WITH_RDP", None);
                    protocols += 1;
                }
                "vnc" => {
                    build.define("SD_WITH_VNC", None);
                    protocols += 1;
                }
                _ => {}
            },
            _ => {}
        }
    }
    if protocols == 0 {
        println!(
            "cargo:warning=the native desktop prefix lists no protocol; the stub backend \
             will be used"
        );
    }
}
