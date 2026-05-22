//! Build automation for barnacle-based kernels.
//!
//! Usage:
//!   cargo xtask check-deps            — verify required tools are installed
//!   cargo xtask iso [--kernel <elf>]  — build a bootable GRUB ISO
//!   cargo xtask run [--kernel <elf>]  — build ISO and boot in QEMU
//!
//! Default kernel ELF: target/x86_64-crusty_os/debug/test_kernel
//! Override with --kernel path/to/kernel.elf
//!
//! On macOS, grub-mkrescue is not available natively.  xtask falls back to
//! Docker automatically if grub-mkrescue is absent but docker is present.

use std::{
    env, fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn main() {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("check-deps") => check_deps(),
        Some("iso")        => cmd_iso(parse_kernel_arg(args)),
        Some("run")        => cmd_run(parse_kernel_arg(args)),
        other => {
            eprintln!("usage: cargo xtask <check-deps | iso | run> [--kernel <elf>]");
            if let Some(cmd) = other {
                eprintln!("unknown subcommand: {cmd}");
            }
            std::process::exit(1);
        }
    }
}

fn parse_kernel_arg(mut args: impl Iterator<Item = String>) -> Option<PathBuf> {
    while let Some(arg) = args.next() {
        if arg == "--kernel" {
            return args.next().map(PathBuf::from);
        }
    }
    None
}

// ── Dependency check ──────────────────────────────────────────────────────────

fn check_deps() {
    let mut ok = true;

    ok &= check_tool("nasm",               &["--version"]);
    ok &= check_tool("qemu-system-x86_64", &["--version"]);

    let has_grub   = tool_exists("grub-mkrescue");
    let has_docker = tool_exists("docker");

    if has_grub {
        println!("  [ok] grub-mkrescue (native)");
    } else if has_docker {
        println!("  [ok] docker (grub-mkrescue fallback)");
    } else {
        eprintln!("  [missing] grub-mkrescue  — install grub or docker");
        ok = false;
    }

    if !ok {
        std::process::exit(1);
    }
    println!("All required tools are present.");
}

fn check_tool(name: &str, version_args: &[&str]) -> bool {
    if tool_exists_with_args(name, version_args) {
        println!("  [ok] {name}");
        true
    } else {
        eprintln!("  [missing] {name}");
        false
    }
}

fn tool_exists(name: &str) -> bool {
    tool_exists_with_args(name, &["--version"])
}

fn tool_exists_with_args(name: &str, args: &[&str]) -> bool {
    Command::new(name)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ── iso ───────────────────────────────────────────────────────────────────────

fn cmd_iso(kernel_override: Option<PathBuf>) {
    let kernel_elf = resolve_kernel(kernel_override);
    build_iso(&kernel_elf);
}

fn cmd_run(kernel_override: Option<PathBuf>) {
    let kernel_elf = resolve_kernel(kernel_override);
    build_test_kernel();
    build_iso(&kernel_elf);
    run_qemu();
}

fn resolve_kernel(override_path: Option<PathBuf>) -> PathBuf {
    if let Some(p) = override_path {
        return p;
    }
    // Default: test_kernel built for the workspace target.
    workspace_root()
        .join("target/x86_64-crusty_os/debug/test_kernel")
}

fn build_test_kernel() {
    let root = workspace_root();
    let status = Command::new("cargo")
        .args(["build", "-p", "test_kernel"])
        .current_dir(&root)
        .status()
        .expect("failed to run cargo build");
    assert!(status.success(), "cargo build -p test_kernel failed");
}

fn build_iso(kernel_elf: &Path) {
    assert!(
        kernel_elf.exists(),
        "kernel ELF not found at {}\nDid you run `cargo build -p test_kernel` first?",
        kernel_elf.display()
    );

    let root    = workspace_root();
    let iso_dir = root.join("isoroot");
    let grub_dir = iso_dir.join("boot/grub");
    let iso_out  = root.join("barnacle.iso");

    // Prepare ISO directory tree.
    fs::create_dir_all(&grub_dir).expect("failed to create isoroot/boot/grub");
    fs::copy(kernel_elf, iso_dir.join("boot/kernel.elf"))
        .expect("failed to copy kernel ELF");

    // Write grub.cfg.
    let cfg_path = grub_dir.join("grub.cfg");
    let mut f = fs::File::create(&cfg_path).expect("failed to create grub.cfg");
    writeln!(f, "set timeout=0").unwrap();
    writeln!(f, "set default=0").unwrap();
    writeln!(f).unwrap();
    writeln!(f, r#"menuentry "barnacle test_kernel" {{"#).unwrap();
    writeln!(f, "    multiboot2 /boot/kernel.elf").unwrap();
    writeln!(f, "    boot").unwrap();
    writeln!(f, "}}").unwrap();

    // Build the ISO using grub-mkrescue (native) or Docker fallback.
    if tool_exists("grub-mkrescue") {
        grub_mkrescue_native(&iso_dir, &iso_out);
    } else if tool_exists("docker") {
        grub_mkrescue_docker(&iso_dir, &iso_out, &root);
    } else {
        eprintln!("error: neither grub-mkrescue nor docker found.");
        eprintln!("  macOS: brew install i386-elf-grub xorriso   OR   install docker");
        eprintln!("  Linux: apt install grub-pc-bin xorriso");
        std::process::exit(1);
    }

    println!("ISO built: {}", iso_out.display());
}

fn grub_mkrescue_native(iso_dir: &Path, iso_out: &Path) {
    let status = Command::new("grub-mkrescue")
        .args(["-o", iso_out.to_str().unwrap(), iso_dir.to_str().unwrap()])
        .status()
        .expect("grub-mkrescue failed to start");
    assert!(status.success(), "grub-mkrescue failed");
}

fn grub_mkrescue_docker(iso_dir: &Path, iso_out: &Path, workspace: &Path) {
    // Mount workspace root into /work inside the container.
    let iso_dir_rel  = iso_dir.strip_prefix(workspace).unwrap().to_str().unwrap().to_owned();
    let iso_out_rel  = iso_out.strip_prefix(workspace).unwrap().to_str().unwrap().to_owned();
    let mount        = format!("{}:/work", workspace.display());

    let status = Command::new("docker")
        .args([
            "run", "--rm",
            "--platform", "linux/amd64",
            "-v", &mount,
            "ubuntu:22.04",
            "sh", "-c",
            &format!(
                "apt-get update -qq && apt-get install -y -qq grub-pc-bin xorriso 2>/dev/null \
                 && grub-mkrescue -o /work/{iso_out_rel} /work/{iso_dir_rel}"
            ),
        ])
        .status()
        .expect("docker failed to start");
    assert!(status.success(), "grub-mkrescue via Docker failed");
}

// ── QEMU ──────────────────────────────────────────────────────────────────────

fn run_qemu() {
    let iso = workspace_root().join("barnacle.iso");
    assert!(iso.exists(), "barnacle.iso not found; run `cargo xtask iso` first");

    Command::new("qemu-system-x86_64")
        .args([
            "-cdrom",    iso.to_str().unwrap(),
            "-serial",   "stdio",
            "-no-reboot",
            "-no-shutdown",
            "-m",        "128M",
        ])
        .spawn()
        .expect("qemu-system-x86_64 failed to start");

    println!("QEMU launched — close the window when done.");
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is xtask's own manifest dir; the workspace root is
    // one level up.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has no parent directory")
        .to_owned()
}
