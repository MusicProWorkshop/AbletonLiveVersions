fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        // Resource id 1: the tray loads its icon from here too.
        res.set_icon_with_id("assets/icon.ico", "1");
        res.set_manifest_file("assets/app.manifest");
        res.set("ProductName", "Live Versions");
        res.set("FileDescription", "Live Versions — version manager for Ableton Live sets");
        res.set("LegalCopyright", "Copyright © 2026 Ishai Hachlili. GPL-3.0-or-later.");
        res.compile().expect("failed to embed Windows resources");
    }
}
