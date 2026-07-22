// Windows resource embedding.
//
// Turns the Windows binary into a "full app": the executable carries the ReCast
// icon (shown in Explorer, the taskbar, Alt-Tab and the file's Properties) and a
// VERSIONINFO block (product name, version, copyright). This is the Windows
// analogue of the macOS .app bundle's Info.plist + AppIcon.icns — Windows has no
// bundle format, so the identity lives inside the .exe itself.
//
// Only runs when the *target* is Windows, so Linux/macOS builds are unaffected.
// On a native MSVC build winresource uses rc.exe automatically; when
// cross-compiling with the GNU (mingw-w64) toolchain we point it at the
// prefixed windres/ar.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    // Rebuild if the icon changes.
    println!("cargo:rerun-if-changed=assets/recast.ico");
    println!("cargo:rerun-if-changed=build.rs");

    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/recast.ico");
    res.set("ProductName", "ReCast");
    res.set("FileDescription", "ReCast — automatic English/Hebrew keyboard-layout correction");
    res.set("OriginalFilename", "ReCast.exe");
    res.set("LegalCopyright", "© 2026 ReCast");
    // FileVersion / ProductVersion default to CARGO_PKG_VERSION, filled in by
    // winresource from the environment.

    // Cross-compiling from a non-Windows host with the GNU toolchain: use the
    // mingw-w64 tools by their target-prefixed names.
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env == "gnu" && !cfg!(target_os = "windows") {
        res.set_windres_path("x86_64-w64-mingw32-windres");
        res.set_ar_path("x86_64-w64-mingw32-ar");
    }

    if let Err(e) = res.compile() {
        // Don't hard-fail the build if the resource compiler is unavailable —
        // the binary still works, it just lacks the embedded icon/metadata.
        println!("cargo:warning=failed to embed Windows resources: {e}");
    }
}
