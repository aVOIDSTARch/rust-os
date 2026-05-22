fn main() {
    // Multiboot2/GRUB path: barnacle exposes boot.o and kernel.ld via DEP_ vars.
    if std::env::var("CARGO_FEATURE_BOOT_MULTIBOOT2").is_ok() {
        let boot_obj = std::env::var("DEP_BARNACLE_BOOT_BOOT_OBJ")
            .expect("DEP_BARNACLE_BOOT_BOOT_OBJ not set — barnacle must be a dep");
        let linker_script = std::env::var("DEP_BARNACLE_BOOT_LINKER_SCRIPT")
            .expect("DEP_BARNACLE_BOOT_LINKER_SCRIPT not set — barnacle must be a dep");

        println!("cargo:rustc-link-arg={boot_obj}");
        println!("cargo:rustc-link-arg=-T{linker_script}");
        println!("cargo:rustc-link-arg=--gc-sections");
    }

    // Limine path: use the simple higher-half linker script (no LMA/VMA split).
    if std::env::var("CARGO_FEATURE_BOOT_LIMINE").is_ok() {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        println!("cargo:rustc-link-arg=-T{manifest}/limine.ld");
        println!("cargo:rustc-link-arg=--gc-sections");
        println!("cargo:rerun-if-changed=limine.ld");
    }
}
