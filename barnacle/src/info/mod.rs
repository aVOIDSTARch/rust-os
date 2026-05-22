//! Boot information provided by GRUB via the Multiboot2 protocol.

pub mod memory;
pub use memory::*;

use multiboot2::BootInformation;

/// Parsed Multiboot2 boot information.
///
/// Wraps [`multiboot2::BootInformation`] with ergonomic accessors.  Obtained
/// via [`crate::entry_point!`]; do not construct directly.
pub struct BootInfo {
    inner: BootInformation<'static>,
}

impl BootInfo {
    pub(crate) fn new(inner: BootInformation<'static>) -> Self {
        Self { inner }
    }

    /// Physical memory map provided by GRUB.
    ///
    /// Returns `None` if GRUB did not include a memory-map tag (should not
    /// happen with a spec-compliant bootloader).
    pub fn memory_map(&self) -> Option<&multiboot2::MemoryMapTag> {
        self.inner.memory_map_tag()
    }

    /// Kernel command-line string, if present.
    pub fn command_line(&self) -> Option<&str> {
        self.inner
            .command_line_tag()
            .and_then(|t| t.cmdline().ok())
    }

    /// Framebuffer information, if GRUB satisfied the framebuffer request.
    ///
    /// Returns `None` if the tag is absent or the framebuffer type is unknown.
    pub fn framebuffer(&self) -> Option<&multiboot2::FramebufferTag> {
        self.inner.framebuffer_tag()?.ok()
    }

    /// ACPI 1.0 RSDP tag, if present.
    pub fn rsdp_v1(&self) -> Option<&multiboot2::RsdpV1Tag> {
        self.inner.rsdp_v1_tag()
    }

    /// ACPI 2.0 RSDP tag (XSDT), if present.
    pub fn rsdp_v2(&self) -> Option<&multiboot2::RsdpV2Tag> {
        self.inner.rsdp_v2_tag()
    }

    /// ELF section headers, if present.
    pub fn elf_sections(&self) -> Option<&multiboot2::ElfSectionsTag> {
        self.inner.elf_sections_tag()
    }

    /// Start address of the raw Multiboot2 information structure.
    pub fn start_address(&self) -> usize {
        self.inner.start_address()
    }

    /// End address of the raw Multiboot2 information structure.
    pub fn end_address(&self) -> usize {
        self.inner.end_address()
    }
}
