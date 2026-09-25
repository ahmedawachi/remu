//! Embeds the icon and version metadata Windows reads out of an `.exe`.
//!
//! The icon is what Explorer, the zip and any shortcut show before Remu runs;
//! the running window's own icon is set by the app itself, from the same mark.
//! The version block is where Task Manager and the firewall prompt get the
//! name "Remu" rather than a file name. It does not change what SmartScreen
//! shows for an unsigned download — that is the file name and "Unknown
//! publisher" either way, and only a code signature changes it.

fn main() {
    // Declaring any of these replaces cargo's default of re-running on every
    // change in the package, so each input has to be named: the icon, and the
    // variables winresource reads to find a resource compiler. Cargo tracks
    // the ones it sets itself, the package version among them.
    println!("cargo:rerun-if-changed=../../packaging/windows/remu.ico");
    println!("cargo:rerun-if-changed=build.rs");
    for var in ["RC_PATH", "WINDRES", "CROSS_COMPILE", "AR"] {
        println!("cargo:rerun-if-env-changed={var}");
    }

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
        // A copyright notice, as the field means and as LICENSE-MIT words it;
        // the licence itself is in the files shipped beside the executable.
        .set("LegalCopyright", "Copyright (c) 2026 The Remu contributors");

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
