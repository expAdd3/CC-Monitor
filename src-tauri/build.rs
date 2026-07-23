fn main() {
    // SQLx embeds migrations at compile time. Cargo does not otherwise know
    // that changing one of these files must rebuild the desktop crate.
    println!("cargo:rerun-if-changed=../crates/monitor-storage/migrations");
    tauri_build::build();
}
