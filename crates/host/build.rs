//! Embeds the tablet helper and tablet app in the host when they've been built, so the finished
//! program is one file. Without them it still builds, and looks for them on disk at run time.

use std::path::Path;

#[path = "src/icon.rs"]
mod icon;

// Windows picks the closest size for each place it shows the logo.
const ICON_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const LANG_EN_US: u16 = 0x0409;

/// Appends one entry to a compiled resource file.
fn resource_entry(out: &mut Vec<u8>, kind: u16, id: u16, flags: u16, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&32u32.to_le_bytes());
    for word in [0xFFFF, kind, 0xFFFF, id] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&LANG_EN_US.to_le_bytes());
    out.extend_from_slice(&[0u8; 8]);
    out.extend_from_slice(data);
    out.resize(out.len().div_ceil(4) * 4, 0);
}

/// One icon image, as the bitmap Windows expects: bottom row first, in BGRA, then an empty mask.
fn icon_bitmap(size: u32) -> Vec<u8> {
    let rgba = icon::ring_rgba(size);
    let mut out = Vec::new();
    for value in [40, size, size * 2] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]);
    for row in rgba.chunks_exact(size as usize * 4).rev() {
        for pixel in row.chunks_exact(4) {
            out.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
    }
    let mask_row = (size as usize).div_ceil(32) * 4;
    out.resize(out.len() + mask_row * size as usize, 0);
    out
}

/// Builds the logo into a compiled resource file by hand, which the linker takes directly. That
/// avoids needing the Windows SDK's resource compiler, and keeps the exe's logo the tray's logo.
fn embed_icon() {
    let mut res = Vec::new();
    // A resource file starts with an empty entry.
    res.extend_from_slice(&0u32.to_le_bytes());
    res.extend_from_slice(&32u32.to_le_bytes());
    for word in [0xFFFFu16, 0, 0xFFFF, 0] {
        res.extend_from_slice(&word.to_le_bytes());
    }
    res.extend_from_slice(&[0u8; 16]);

    let mut group = Vec::new();
    for word in [0u16, 1, ICON_SIZES.len() as u16] {
        group.extend_from_slice(&word.to_le_bytes());
    }
    for (index, size) in ICON_SIZES.into_iter().enumerate() {
        let id = index as u16 + 1;
        let bitmap = icon_bitmap(size);
        resource_entry(&mut res, RT_ICON, id, 0x1010, &bitmap);
        // 256 doesn't fit in a byte, so the format writes it as 0.
        let side = if size == 256 { 0 } else { size as u8 };
        group.extend_from_slice(&[side, side, 0, 0]);
        group.extend_from_slice(&1u16.to_le_bytes());
        group.extend_from_slice(&32u16.to_le_bytes());
        group.extend_from_slice(&(bitmap.len() as u32).to_le_bytes());
        group.extend_from_slice(&id.to_le_bytes());
    }
    resource_entry(&mut res, RT_GROUP_ICON, 1, 0x1030, &group);

    let path = Path::new(&std::env::var("OUT_DIR").unwrap()).join("osutab-icon.res");
    std::fs::write(&path, res).unwrap();
    println!("cargo:rustc-link-arg-bins={}", path.display());
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
}

fn main() {
    embed_icon();
    println!("cargo:rustc-check-cfg=cfg(embedded_assets)");
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let root = Path::new(&manifest).join("../..");
    let helper = root.join("target/aarch64-unknown-linux-musl/release/osutab-helper");
    let apk = root.join("android/app/build/outputs/apk/debug/app-debug.apk");
    println!("cargo:rerun-if-changed={}", helper.display());
    println!("cargo:rerun-if-changed={}", apk.display());
    if helper.exists() && apk.exists() {
        println!("cargo:rustc-cfg=embedded_assets");
        println!("cargo:rustc-env=OSUTAB_HELPER_PATH={}", helper.canonicalize().unwrap().display());
        println!("cargo:rustc-env=OSUTAB_APK_PATH={}", apk.canonicalize().unwrap().display());
    }

    println!("cargo:rustc-check-cfg=cfg(embedded_bridge)");
    let bridge = root.join("otd-plugin/bin/Release/net10.0/OsutabBridge.dll");
    println!("cargo:rerun-if-changed={}", bridge.display());
    if bridge.exists() {
        println!("cargo:rustc-cfg=embedded_bridge");
        println!("cargo:rustc-env=OSUTAB_BRIDGE_PATH={}", bridge.canonicalize().unwrap().display());
    }
    println!("cargo:rerun-if-changed={}", root.join("otd-plugin/osutab S Pen.json").display());
}
