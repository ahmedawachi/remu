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
        // Hard failure only where a resource compiler is genuinely expected: a
        // Windows host building for MSVC, which is what the release runner is.
        // That is narrower than `cfg!(windows)` on purpose — the MSVC path
        // wants `rc.exe` from the Windows SDK, while the `-gnu` path wants
        // `windres` from a MinGW install that a contributor may not have, and
        // failing their build over a missing icon would be gatekeeping.
        //
        // Everywhere else this is said out loud rather than swallowed, because
        // a cross-compiled release artefact must not ship without its icon and
        // version block and nobody would otherwise notice.
        let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
        if cfg!(windows) && target_env == "msvc" {
            panic!("could not embed the Windows resources: {err}");
        }
        println!(
            "cargo:warning=no Windows resource compiler ({err}); \
             remu.exe will have no icon and no version metadata"
        );
    }
}
