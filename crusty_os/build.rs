fn main() {
    if std::env::var("CARGO_FEATURE_USE_BARNACLE").is_ok() {
        let boot_obj = std::env::var("DEP_BARNACLE_BOOT_BOOT_OBJ")
            .expect("DEP_BARNACLE_BOOT_BOOT_OBJ not set — barnacle must be a dependency");
        let linker_script = std::env::var("DEP_BARNACLE_BOOT_LINKER_SCRIPT")
            .expect("DEP_BARNACLE_BOOT_LINKER_SCRIPT not set — barnacle must be a dependency");

        println!("cargo:rustc-link-arg={boot_obj}");
        println!("cargo:rustc-link-arg=-T{linker_script}");
        println!("cargo:rustc-link-arg=--gc-sections");
    }
}
