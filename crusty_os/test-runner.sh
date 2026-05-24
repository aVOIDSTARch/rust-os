#!/bin/bash
# Cargo test runner for barnacle (Multiboot2) kernels.
#
# Creates a GRUB-bootable ISO from the test ELF, boots it in QEMU, and
# maps the ISA debug-exit codes to standard exit codes:
#   33 (0x10 written to port 0xf4) → exit 0  (success)
#   35 (0x11 written to port 0xf4) → exit 1  (failure)
#
# Requires: qemu-system-x86_64 + (grub-mkrescue OR docker)
# The Docker image is built once and cached as barnacle-iso-builder:latest.

set -euo pipefail

KERNEL="$1"
DOCKER_IMAGE="barnacle-iso-builder:latest"

# Test binaries produced by `cargo test` have a trailing -<hex> hash in their
# name (e.g. crusty_os-abc1234). Regular `cargo run` binaries do not.
# Show the QEMU display only for non-test runs so automated test runs stay
# headless; test windows would flash briefly for each integration test.
BINARY_NAME="$(basename "$KERNEL")"
if [[ "$BINARY_NAME" == *"-"* ]]; then
    DISPLAY_OPT="-display none"
else
    DISPLAY_OPT="-display cocoa"
fi

# ── Build Docker ISO-builder image (one-time, cached) ────────────────────────

if ! docker image inspect "$DOCKER_IMAGE" &>/dev/null; then
    echo "[test-runner] Building Docker image $DOCKER_IMAGE (one-time setup)..." >&2
    docker build --platform linux/amd64 -t "$DOCKER_IMAGE" - << 'DOCKERFILE' >&2
FROM --platform=linux/amd64 ubuntu:22.04
RUN apt-get update -qq \
 && apt-get install -y -qq --no-install-recommends grub-pc-bin xorriso \
 && rm -rf /var/lib/apt/lists/*
DOCKERFILE
fi

# ── Create temp workspace ─────────────────────────────────────────────────────

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

mkdir -p "$TMPDIR/isoroot/boot/grub"
cp "$KERNEL" "$TMPDIR/isoroot/boot/kernel.elf"

cat > "$TMPDIR/isoroot/boot/grub/grub.cfg" << 'GRUBCFG'
set timeout=0
set default=0

menuentry "crusty_os test" {
    multiboot2 /boot/kernel.elf
    boot
}
GRUBCFG

# ── Build ISO ─────────────────────────────────────────────────────────────────

docker run --rm --platform linux/amd64 \
    -v "$TMPDIR:/work" \
    "$DOCKER_IMAGE" \
    grub-mkrescue -o /work/test.iso /work/isoroot 2>/dev/null

# ── Boot ISO in QEMU ──────────────────────────────────────────────────────────

set +e
# shellcheck disable=SC2086
qemu-system-x86_64 \
    -cdrom "$TMPDIR/test.iso" \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -serial stdio \
    $DISPLAY_OPT \
    -no-reboot \
    -m 128M
rc=$?
set -e

case $rc in
    33) exit 0 ;;
    35) exit 1 ;;
    *)  exit $rc ;;
esac
