//! Embeds the icon and version metadata Windows reads out of an `.exe`.
//!
//! Without this the app is a generic-icon window in the taskbar and an
//! unnamed row in Task Manager, and the SmartScreen prompt on first run has
//! nothing to show but a filename — which is exactly the moment a user decides
//! whether to trust the download.

fn main() {
    // The icon is the only input, so nothing else needs to re-run this.
    println!("cargo:rerun-if-changed=../../packaging/windows/remu.ico");
    println!("cargo:rerun-if-changed=build.rs");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let icon = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packaging/windows/remu.ico"
    );

    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon(icon)
        // Windows shows `FileDescription`, not the product name, as the
        // application name in Task Manager and the firewall prompt, and the
        // crate name `remu-desk` is not what a user should read there.
        .set("FileDescription", "Remu")
        .set("ProductName", "Remu")
        .set("OriginalFilename", "remu.exe")
        .set("LegalCopyright", "MIT OR Apache-2.0");

    if let Err(err) = resource.compile() {
        // Cross-compiling to Windows from another host has no `rc.exe`, and a
        // missing icon is not worth failing that build over. Said out loud
        // rather than swallowed, because the release artifact must not ship
        // like this: on a Windows host the resource compiler is present and a
        // failure here is real.
        if cfg!(windows) {
            panic!("could not embed the Windows resources: {err}");
        }
        println!("cargo:warning=building for Windows without a resource compiler; the executable will have no icon or version metadata");
    }
}
