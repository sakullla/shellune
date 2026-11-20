//! Compiles the small C surface that is still shared with the native tests: the
//! ABI version, the status/state/failure names and the text conversions a probe
//! calls.
//!
//! Neither protocol runs through this shim any more. RDP and VNC are implemented
//! in Rust (`src/rdp`, `src/vnc`) and the crate links no protocol library, so
//! `sd_vnc.c` and the vendored LibVNCClient are not part of the build at all.
//! `sd_stub.c` supplies the backend entry points the C façade still declares;
//! nothing in the crate calls them.

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let mut build = cc::Build::new();
    for file in ["sd_shim.c", "sd_clip.c", "sd_stub.c"] {
        let path = manifest_dir.join("shim").join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        build.file(path);
    }
    for header in ["sd_internal.h", "shellune_desktop_shim.h"] {
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir.join("shim").join(header).display()
        );
    }
    build.include(manifest_dir.join("shim"));
    build.define("_CRT_SECURE_NO_WARNINGS", None);
    build.warnings(false);
    build.compile("shellune_desktop_shim");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        println!("cargo:rustc-link-lib=pthread");
    }
}
