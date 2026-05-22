//! Build automation for barnacle-based kernels.
//!
//! Usage:
//!   cargo xtask check-deps                        — verify required tools
//!   cargo xtask iso [--kernel <elf>] [--boot <b>] — build a bootable ISO
//!   cargo xtask run [--kernel <elf>] [--boot <b>] — build ISO and boot in QEMU
//!
//! --boot values:
//!   grub        (default) builds test_kernel, GRUB/Multiboot2 ISO
//!   multiboot2            builds crusty_os --features boot-multiboot2, GRUB ISO
//!   limine                builds crusty_os --features boot-limine,     Limine ISO
//!
//! On macOS, grub-mkrescue is not available natively.  xtask falls back to
//! Docker automatically if grub-mkrescue is absent but docker is present.
//! Limine ISO creation similarly falls back to Docker.

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
        Some("iso") => {
            let (boot, kernel) = parse_args(args);
            cmd_iso(boot, kernel);
        }
        Some("run") => {
            let (boot, kernel) = parse_args(args);
            cmd_run(boot, kernel);
        }
        other => {
            eprintln!("usage: cargo xtask <check-deps | iso | run> [--kernel <elf>] [--boot grub|multiboot2|limine]");
            if let Some(cmd) = other {
                eprintln!("unknown subcommand: {cmd}");
            }
            std::process::exit(1);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Boot {
    Grub,
    Multiboot2,
    Limine,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> (Boot, Option<PathBuf>) {
    let mut boot   = Boot::Grub;
    let mut kernel = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--boot" => match args.next().as_deref() {
                Some("grub")        => boot = Boot::Grub,
                Some("multiboot2")  => boot = Boot::Multiboot2,
                Some("limine")      => boot = Boot::Limine,
                other => {
                    eprintln!("--boot expects grub|multiboot2|limine, got {:?}", other);
                    std::process::exit(1);
                }
            },
            "--kernel" => { kernel = args.next().map(PathBuf::from); }
            _ => {}
        }
    }
    (boot, kernel)
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

    // Limine tooling (optional — only needed for --boot limine)
    let has_limine  = tool_exists("limine");
    let has_xorriso = tool_exists("xorriso");
    if has_limine && has_xorriso {
        println!("  [ok] limine + xorriso (Limine ISO support)");
    } else if has_docker {
        println!("  [ok] docker (Limine ISO fallback)");
    } else {
        println!("  [warn] limine/xorriso not found — `--boot limine` will require docker");
    }

    if !ok { std::process::exit(1); }
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

// ── iso / run commands ────────────────────────────────────────────────────────

fn cmd_iso(boot: Boot, kernel_override: Option<PathBuf>) {
    let kernel_elf = resolve_kernel(boot, kernel_override);
    build_iso(boot, &kernel_elf);
}

fn cmd_run(boot: Boot, kernel_override: Option<PathBuf>) {
    let kernel_elf = resolve_kernel(boot, kernel_override);
    build_kernel(boot);
    build_iso(boot, &kernel_elf);
    run_qemu(boot);
}

fn resolve_kernel(boot: Boot, override_path: Option<PathBuf>) -> PathBuf {
    if let Some(p) = override_path { return p; }
    let root = workspace_root();
    match boot {
        Boot::Grub =>
            root.join("target/x86_64-crusty_os/debug/test_kernel"),
        Boot::Multiboot2 | Boot::Limine =>
            root.join("target/x86_64-crusty_os/debug/crusty_os"),
    }
}

fn build_kernel(boot: Boot) {
    let root = workspace_root();
    let (pkg, extra_args): (&str, &[&str]) = match boot {
        Boot::Grub =>
            ("test_kernel", &[]),
        Boot::Multiboot2 =>
            ("crusty_os", &["--no-default-features", "--features", "boot-multiboot2"]),
        Boot::Limine =>
            ("crusty_os", &["--no-default-features", "--features", "boot-limine"]),
    };

    let mut cmd = Command::new("cargo");
    cmd.arg("build").arg("-p").arg(pkg);
    cmd.args(extra_args);
    cmd.current_dir(&root);

    let status = cmd.status().expect("failed to run cargo build");
    assert!(status.success(), "cargo build failed for {pkg}");
}

fn build_iso(boot: Boot, kernel_elf: &Path) {
    match boot {
        Boot::Grub | Boot::Multiboot2 => build_grub_iso(kernel_elf),
        Boot::Limine                  => build_limine_iso(kernel_elf),
    }
}

// ── GRUB ISO (grub and multiboot2 boot paths) ─────────────────────────────────

fn build_grub_iso(kernel_elf: &Path) {
    assert!(
        kernel_elf.exists(),
        "kernel ELF not found at {}\nDid you forget to build?",
        kernel_elf.display()
    );

    let root     = workspace_root();
    let iso_dir  = root.join("isoroot");
    let grub_dir = iso_dir.join("boot/grub");
    let iso_out  = root.join("barnacle.iso");

    fs::create_dir_all(&grub_dir).expect("failed to create isoroot/boot/grub");
    fs::copy(kernel_elf, iso_dir.join("boot/kernel.elf"))
        .expect("failed to copy kernel ELF");

    let cfg_path = grub_dir.join("grub.cfg");
    let mut f = fs::File::create(&cfg_path).expect("failed to create grub.cfg");
    writeln!(f, "set timeout=0").unwrap();
    writeln!(f, "set default=0").unwrap();
    writeln!(f).unwrap();
    writeln!(f, r#"menuentry "barnacle test_kernel" {{"#).unwrap();
    writeln!(f, "    multiboot2 /boot/kernel.elf").unwrap();
    writeln!(f, "    boot").unwrap();
    writeln!(f, "}}").unwrap();

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

const GRUB_DOCKER_IMAGE: &str = "barnacle-iso-builder:latest";

fn ensure_grub_docker_image() {
    let exists = Command::new("docker")
        .args(["image", "inspect", GRUB_DOCKER_IMAGE])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if exists { return; }

    println!("Building Docker image {GRUB_DOCKER_IMAGE} (one-time setup) …");
    let dockerfile = "\
FROM --platform=linux/amd64 ubuntu:22.04\n\
RUN apt-get update -qq \
 && apt-get install -y -qq --no-install-recommends grub-pc-bin xorriso \
 && rm -rf /var/lib/apt/lists/*\n";

    let mut child = Command::new("docker")
        .args(["build", "--platform", "linux/amd64", "-t", GRUB_DOCKER_IMAGE, "-"])
        .stdin(Stdio::piped())
        .spawn()
        .expect("docker build failed to start");
    child.stdin.take().unwrap().write_all(dockerfile.as_bytes()).unwrap();
    let status = child.wait().expect("docker build failed");
    assert!(status.success(), "docker build of {GRUB_DOCKER_IMAGE} failed");
}

fn grub_mkrescue_docker(iso_dir: &Path, iso_out: &Path, workspace: &Path) {
    ensure_grub_docker_image();

    let iso_dir_rel = iso_dir.strip_prefix(workspace).unwrap().to_str().unwrap().to_owned();
    let iso_out_rel = iso_out.strip_prefix(workspace).unwrap().to_str().unwrap().to_owned();
    let mount       = format!("{}:/work", workspace.display());

    let status = Command::new("docker")
        .args([
            "run", "--rm",
            "--platform", "linux/amd64",
            "-v", &mount,
            GRUB_DOCKER_IMAGE,
            "grub-mkrescue", "-o",
            &format!("/work/{iso_out_rel}"),
            &format!("/work/{iso_dir_rel}"),
        ])
        .status()
        .expect("docker run failed to start");
    assert!(status.success(), "grub-mkrescue via Docker failed");
}

// ── Limine ISO ────────────────────────────────────────────────────────────────

fn build_limine_iso(kernel_elf: &Path) {
    assert!(
        kernel_elf.exists(),
        "kernel ELF not found at {}\nDid you forget to build?",
        kernel_elf.display()
    );

    let root    = workspace_root();
    let iso_dir = root.join("limine_isoroot");
    let iso_out = root.join("limine.iso");

    // Directory structure expected by Limine:
    //   limine_isoroot/
    //     boot/
    //       kernel.elf
    //       limine.cfg
    //       limine/
    //         limine-bios.sys
    //         limine-bios-cd.bin
    //         limine-uefi-cd.bin
    let limine_dir = iso_dir.join("boot/limine");
    fs::create_dir_all(&limine_dir).expect("failed to create limine_isoroot/boot/limine");
    fs::copy(kernel_elf, iso_dir.join("boot/kernel.elf"))
        .expect("failed to copy kernel ELF");

    let cfg_path = iso_dir.join("boot/limine.cfg");
    let mut f = fs::File::create(&cfg_path).expect("failed to create limine.cfg");
    writeln!(f, "TIMEOUT=0").unwrap();
    writeln!(f).unwrap();
    writeln!(f, "[crusty_os]").unwrap();
    writeln!(f, "PROTOCOL=limine").unwrap();
    writeln!(f, "PATH=boot:///boot/kernel.elf").unwrap();

    let has_limine  = tool_exists("limine");
    let has_xorriso = tool_exists("xorriso");

    if has_limine && has_xorriso {
        limine_iso_native(&iso_dir, &iso_out, &limine_dir);
    } else if tool_exists("docker") {
        limine_iso_docker(&iso_dir, &iso_out, &root);
    } else {
        eprintln!("error: limine ISO requires limine+xorriso or docker.");
        eprintln!("  macOS: brew install limine xorriso");
        eprintln!("  Linux: apt install limine xorriso");
        eprintln!("  Or:    install docker");
        std::process::exit(1);
    }

    println!("Limine ISO built: {}", iso_out.display());
}

fn limine_iso_native(iso_dir: &Path, iso_out: &Path, limine_dir: &Path) {
    // Find Limine's system files. `limine --sysroot` or known brew paths.
    let limine_share = find_limine_share();

    for f in &["limine-bios.sys", "limine-bios-cd.bin", "limine-uefi-cd.bin"] {
        let src = limine_share.join(f);
        assert!(src.exists(), "Limine file not found: {}", src.display());
        fs::copy(&src, limine_dir.join(f)).expect("failed to copy Limine file");
    }

    let status = Command::new("xorriso")
        .args([
            "-as", "mkisofs",
            "-b", "boot/limine/limine-bios-cd.bin",
            "-no-emul-boot", "-boot-load-size", "4", "-boot-info-table",
            "--efi-boot", "boot/limine/limine-uefi-cd.bin",
            "-o", iso_out.to_str().unwrap(),
            iso_dir.to_str().unwrap(),
        ])
        .status()
        .expect("xorriso failed to start");
    assert!(status.success(), "xorriso failed");

    // Make the ISO BIOS-bootable.
    let status = Command::new("limine")
        .args(["bios-install", iso_out.to_str().unwrap()])
        .status()
        .expect("limine bios-install failed to start");
    assert!(status.success(), "limine bios-install failed");
}

fn find_limine_share() -> PathBuf {
    // Check common install prefixes.
    for prefix in &["/opt/homebrew", "/usr/local", "/usr"] {
        let p = PathBuf::from(prefix).join("share/limine");
        if p.join("limine-bios.sys").exists() { return p; }
    }
    // Fall back to asking `limine` where its data is.
    eprintln!("Cannot locate limine share directory.");
    eprintln!("Install with: brew install limine  OR  apt install limine");
    std::process::exit(1);
}

const LIMINE_DOCKER_IMAGE: &str = "barnacle-limine-builder:latest";

fn ensure_limine_docker_image() {
    let exists = Command::new("docker")
        .args(["image", "inspect", LIMINE_DOCKER_IMAGE])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if exists { return; }

    println!("Building Docker image {LIMINE_DOCKER_IMAGE} (one-time setup) …");
    let dockerfile = "\
FROM --platform=linux/amd64 ubuntu:22.04\n\
RUN apt-get update -qq \
 && apt-get install -y -qq --no-install-recommends limine xorriso \
 && rm -rf /var/lib/apt/lists/*\n";

    let mut child = Command::new("docker")
        .args(["build", "--platform", "linux/amd64", "-t", LIMINE_DOCKER_IMAGE, "-"])
        .stdin(Stdio::piped())
        .spawn()
        .expect("docker build failed to start");
    child.stdin.take().unwrap().write_all(dockerfile.as_bytes()).unwrap();
    let status = child.wait().expect("docker build failed");
    assert!(status.success(), "docker build of {LIMINE_DOCKER_IMAGE} failed");
}

fn limine_iso_docker(iso_dir: &Path, iso_out: &Path, workspace: &Path) {
    ensure_limine_docker_image();

    let iso_dir_rel = iso_dir.strip_prefix(workspace).unwrap().to_str().unwrap().to_owned();
    let iso_out_rel = iso_out.strip_prefix(workspace).unwrap().to_str().unwrap().to_owned();
    let limine_dir_rel = format!("{}/boot/limine", iso_dir_rel);
    let mount = format!("{}:/work", workspace.display());

    // Single docker run: copy Limine files, build ISO, install bootloader.
    let script = format!(
        "cp /usr/share/limine/limine-bios.sys /work/{limine_dir_rel}/ && \
         cp /usr/share/limine/limine-bios-cd.bin /work/{limine_dir_rel}/ && \
         cp /usr/share/limine/limine-uefi-cd.bin /work/{limine_dir_rel}/ && \
         xorriso -as mkisofs \
           -b boot/limine/limine-bios-cd.bin \
           -no-emul-boot -boot-load-size 4 -boot-info-table \
           --efi-boot boot/limine/limine-uefi-cd.bin \
           -o /work/{iso_out_rel} /work/{iso_dir_rel} && \
         limine bios-install /work/{iso_out_rel}"
    );

    let status = Command::new("docker")
        .args([
            "run", "--rm",
            "--platform", "linux/amd64",
            "-v", &mount,
            LIMINE_DOCKER_IMAGE,
            "sh", "-c", &script,
        ])
        .status()
        .expect("docker run failed to start");
    assert!(status.success(), "Limine ISO creation via Docker failed");
}

// ── QEMU ──────────────────────────────────────────────────────────────────────

fn run_qemu(boot: Boot) {
    let iso = match boot {
        Boot::Grub | Boot::Multiboot2 => workspace_root().join("barnacle.iso"),
        Boot::Limine                  => workspace_root().join("limine.iso"),
    };
    assert!(iso.exists(), "{} not found; run `cargo xtask iso` first", iso.display());

    Command::new("qemu-system-x86_64")
        .args([
            "-cdrom",    iso.to_str().unwrap(),
            "-serial",   "stdio",
            "-no-reboot",
            "-no-shutdown",
            "-m",        "128M",
            "-display",  "cocoa,zoom-to-fit=on",
        ])
        .spawn()
        .expect("qemu-system-x86_64 failed to start");

    println!("QEMU launched — close the window when done.");
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has no parent directory")
        .to_owned()
}
