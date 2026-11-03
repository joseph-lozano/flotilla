//! Linux: link even without the `-dev` packages GPUI's libraries come from.
//!
//! The linker wants `libxkbcommon-x11.so`, which only `libxkbcommon-x11-dev` installs; a machine
//! with just the library (`libxkbcommon-x11.so.0`, there for any X11 desktop) can't link the app.
//! For each library the link needs, when the plain `.so` is missing but the versioned one is
//! there, this links against the versioned one through a `lib<name>.so` in `OUT_DIR`. The binary
//! still asks for the same `.so.0` at run time. With the `-dev` packages it does nothing.

use std::env;
use std::path::{Path, PathBuf};

/// The system libraries GPUI's Linux platform links by name, with their versioned file.
const LIBS: &[(&str, &str)] = &[
    ("xkbcommon-x11", "libxkbcommon-x11.so.0"),
    ("xkbcommon", "libxkbcommon.so.0"),
    ("xcb", "libxcb.so.1"),
    ("freetype", "libfreetype.so.6"),
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    // Only for this machine's own libraries: a cross build brings its own sysroot.
    if target_os != "linux" || env::var("HOST").ok() != env::var("TARGET").ok() {
        return;
    }
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let triplet = format!("{arch}-linux-gnu");
    let dirs: Vec<PathBuf> = [
        format!("/usr/lib/{triplet}"),
        format!("/lib/{triplet}"),
        "/usr/lib64".into(),
        "/lib64".into(),
        "/usr/lib".into(),
        "/lib".into(),
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("link");

    let mut linked = false;
    for (name, versioned) in LIBS {
        let plain = format!("lib{name}.so");
        if dirs.iter().any(|d| d.join(&plain).exists()) {
            continue;
        }
        let Some(lib) = dirs.iter().map(|d| d.join(versioned)).find(|p| p.exists()) else {
            continue;
        };
        std::fs::create_dir_all(&out).expect("create OUT_DIR/link");
        let link = out.join(&plain);
        let _ = std::fs::remove_file(&link);
        symlink(&lib, &link);
        println!(
            "cargo:warning=no {plain} (from its -dev package): linking {}",
            lib.display()
        );
        linked = true;
    }
    if linked {
        println!("cargo:rustc-link-search=native={}", out.display());
    }
}

#[cfg(unix)]
fn symlink(from: &Path, to: &Path) {
    std::os::unix::fs::symlink(from, to).expect("symlink in OUT_DIR");
}

#[cfg(not(unix))]
fn symlink(_: &Path, _: &Path) {}
