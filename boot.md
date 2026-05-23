// This file contains details on how to boot with different bootloaders.

# Command Line Build Commands

# Multiboot2 / GRUB
cargo build -p crusty_os --no-default-features --features boot-multiboot2

# Limine
cargo build -p crusty_os --no-default-features --features boot-limine

# Legacy bootloader (original default)
cargo build -p crusty_os

- If run is swapped for build, it will both build and run the operating system in the qemu emulator.
