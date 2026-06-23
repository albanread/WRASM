//! Build script for `studio`.
//!
//! Embeds the Windows application manifest (`tools/studio.exe.manifest`) into
//! the binaries so the IDE gets, at process start:
//!   - Per-monitor-v2 DPI awareness (crisp Direct2D on high-DPI displays),
//!   - Common Controls v6 visual styles for the file dialogs,
//!   - UTF-8 active code page (non-ASCII filenames round-trip the A APIs),
//!   - supportedOS GUIDs through Windows 11.
//!
//! On non-Windows builds the embed is skipped — `studio` is `cfg(windows)`.

fn main() {
    println!("cargo:rerun-if-changed=tools/studio.rc");
    println!("cargo:rerun-if-changed=tools/studio.exe.manifest");

    #[cfg(target_os = "windows")]
    {
        embed_resource::compile("tools/studio.rc", embed_resource::NONE)
            .manifest_required()
            .unwrap();
    }
}
