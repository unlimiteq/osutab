//! Embeds the tablet helper and tablet app in the host when they've been built, so the finished
//! program is one file. Without them it still builds, and looks for them on disk at run time.

use std::path::Path;

fn main() {
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
