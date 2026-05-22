use std::{path::PathBuf, process::Command};

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out      = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let asm      = manifest.join("src/boot/boot.asm");
    let obj      = out.join("boot.o");
    let ld       = manifest.join("kernel.ld");

    println!("cargo:rerun-if-changed=src/boot/boot.asm");
    println!("cargo:rerun-if-changed=kernel.ld");

    let ok = Command::new("nasm")
        .args(["-f", "elf64", asm.to_str().unwrap(), "-o", obj.to_str().unwrap()])
        .status()
        .expect("nasm not found — install: brew install nasm  /  apt install nasm");
    assert!(ok.success(), "nasm failed to assemble src/boot/boot.asm");

    // Expose paths via cargo metadata so dependent kernels can link the boot stub.
    // cargo:rustc-link-arg is NOT propagated through library dependencies; kernels
    // must read DEP_BARNACLE_BOOT_BOOT_OBJ and DEP_BARNACLE_BOOT_LINKER_SCRIPT
    // from their own build.rs and emit the link args themselves.
    println!("cargo:boot_obj={}", obj.display());
    println!("cargo:linker_script={}", ld.display());
}
