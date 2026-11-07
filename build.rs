fn main() {
    println!("cargo:rerun-if-changed=assets/icons/shellune.ico");
    println!("cargo:rerun-if-changed=packaging/windows/shellune.rc");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // GPUI loads icon resource 1 for the window and taskbar.
        embed_resource::compile_for(
            "packaging/windows/shellune.rc",
            ["shellune"],
            embed_resource::NONE,
        )
        .manifest_optional()
        .expect("failed to embed the Shellune application icon");
    }
}
