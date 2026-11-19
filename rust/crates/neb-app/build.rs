//! Puts `WebView2Loader.dll` next to the executable.
//!
//! The GNU target links `neb-app.exe` against the loader's import library (the MSVC target links the
//! static loader instead, which is why `webview2-com-sys` only special-cases `target_env = "msvc"`).
//! Windows therefore has to find `WebView2Loader.dll` at startup, and the one place this build
//! controls is the folder the binary is written to.
//!
//! The DLL is not vendored in this repository: `webview2-com-sys` already ships it in its own build
//! output, so the copy below reads that file and leaves the source of truth in the dependency. A
//! release package must include the copy; see `installer/ProjectNeb.iss`.

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // A missing loader is not fatal here: the app still links, and a machine with the loader on
    // PATH does not need the copy. The build output says where to look.
    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        return;
    };

    let out_dir = PathBuf::from(out_dir);
    let Some(profile_dir) = profile_dir(&out_dir) else {
        return;
    };

    let Some(loader) = loader_source(&profile_dir) else {
        println!(
            "cargo:warning=WebView2Loader.dll was not found in the webview2-com-sys build output; \
             copy it next to neb-app.exe before shipping"
        );
        return;
    };

    if let Err(error) = fs::copy(&loader, profile_dir.join("WebView2Loader.dll")) {
        println!("cargo:warning=cannot copy '{}': {error}", loader.display());
    }
}

/// `target/<profile>` from `target/<profile>/build/neb-app/<hash>/out`.
fn profile_dir(out_dir: &Path) -> Option<PathBuf> {
    out_dir
        .ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == "build"))
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

/// The x64 loader `webview2-com-sys` unpacked, if it is there.
fn loader_source(profile_dir: &Path) -> Option<PathBuf> {
    let build_dir = profile_dir.join("build").join("webview2-com-sys");
    let entries = fs::read_dir(build_dir).ok()?;

    for entry in entries.flatten() {
        let candidate = entry
            .path()
            .join("out")
            .join("x64")
            .join("WebView2Loader.dll");

        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}
