//! Re-exports of Multiboot2 memory-map types for use by kernels.

pub use multiboot2::{MemoryAreaType, MemoryMapTag};

/// A single Multiboot2 memory region.
///
/// Re-exported from the `multiboot2` crate so kernels only need to depend
/// on `barnacle`, not `multiboot2` directly.
pub use multiboot2::MemoryArea;
