// Global Descriptor Table (GDT) and Task State Segment (TSS) setup.

use x86_64::VirtAddr;
use x86_64::structures::tss::TaskStateSegment;
use x86_64::structures::gdt::{GlobalDescriptorTable, Descriptor, SegmentSelector};
use lazy_static::lazy_static;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;
pub const NMI_IST_INDEX:          u16 = 1;
pub const MACHINE_CHECK_IST_INDEX: u16 = 2;

// Struct to hold the segment selectors for the code and TSS segments.
struct Selectors {
    code_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

macro_rules! ist_stack {
    ($tss:expr, $index:expr) => {{
        const STACK_SIZE: usize = 4096 * 5;
        static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
        let start = VirtAddr::from_ptr(&raw const STACK);
        $tss.interrupt_stack_table[$index as usize] = start + STACK_SIZE;
    }};
}

// Initializes the TSS.
lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();
        unsafe {
            ist_stack!(tss, DOUBLE_FAULT_IST_INDEX);
            ist_stack!(tss, NMI_IST_INDEX);
            ist_stack!(tss, MACHINE_CHECK_IST_INDEX);
        }
        tss
    };
}

// Initializes the GDT.
lazy_static! {
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        let code_selector = gdt.add_entry(Descriptor::kernel_code_segment());
        let tss_selector = gdt.add_entry(Descriptor::tss_segment(&TSS));
        ( gdt, Selectors {
            code_selector,
            tss_selector,
        })
    };
}

// Loads the GDT
pub fn init() {
    use x86_64::instructions::segmentation::{CS, Segment};
    use x86_64::instructions::tables::load_tss;

    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.code_selector);
        load_tss(GDT.1.tss_selector);
    }
}
