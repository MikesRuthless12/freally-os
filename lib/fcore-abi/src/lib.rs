// SPDX-License-Identifier: Apache-2.0
//! The Freally Core ABI (PRD § 3 *Constants*, *BootInfo*; § 4 *System calls*).
//!
//! Everything here is shared by the bootloader, the kernel and user space, so
//! every structure is `#[repr(C)]`, little-endian, with explicit padding.
#![no_std]

/// The version of this build of Freally OS (the workspace version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A UEFI-style GUID, in its in-memory (mixed-endian) layout.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

impl Guid {
    pub const fn new(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Guid {
        Guid { data1, data2, data3, data4 }
    }
}

/// UEFI variables (`FreallyBootCtl`): 960E8856-D6AF-4DCC-B684-E79C91B69B15.
pub const FREALLY_VENDOR_GUID: Guid =
    Guid::new(0x960E_8856, 0xD6AF, 0x4DCC, [0xB6, 0x84, 0xE7, 0x9C, 0x91, 0xB6, 0x9B, 0x15]);
/// GPT type of a Freally system slot: 681F2A69-CE1E-49CC-9B6D-6BC3C0CF4345.
pub const PART_TYPE_FREALLY_SYSTEM: Guid =
    Guid::new(0x681F_2A69, 0xCE1E, 0x49CC, [0x9B, 0x6D, 0x6B, 0xC3, 0xC0, 0xCF, 0x43, 0x45]);
/// GPT type of the Freally data partition: 2F8C112F-817E-4E65-AE8A-48BE3F1AE32C.
pub const PART_TYPE_FREALLY_DATA: Guid =
    Guid::new(0x2F8C_112F, 0x817E, 0x4E65, [0xAE, 0x8A, 0x48, 0xBE, 0x3F, 0x1A, 0xE3, 0x2C]);

/// The block size of system images and FreallyFS.
pub const BLOCK: usize = 4096;

/// The three build flavours. They are mutually exclusive.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flavour {
    Release = 0,
    Dev = 1,
    E2e = 2,
}

impl Flavour {
    pub const fn from_u32(v: u32) -> Option<Flavour> {
        match v {
            0 => Some(Flavour::Release),
            1 => Some(Flavour::Dev),
            2 => Some(Flavour::E2e),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Flavour::Release => "release",
            Flavour::Dev => "dev",
            Flavour::E2e => "e2e",
        }
    }
}

/// A pointer and a length, as the bootloader hands them to the kernel.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Slice<T> {
    pub ptr: *const T,
    pub len: u64,
}

/// What a range of physical memory is.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemKind {
    Usable = 0,
    Reserved = 1,
    AcpiReclaim = 2,
    AcpiNvs = 3,
    Mmio = 4,
    Image = 5,
    BootInfo = 6,
    RuntimeCode = 7,
    RuntimeData = 8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MemRegion {
    pub base: u64,
    pub len: u64,
    pub kind: MemKind,
    pub _pad: u32,
}

#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelFormat {
    Bgrx8888 = 0,
    Rgbx8888 = 1,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Framebuffer {
    pub base: u64,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub stride_px: u32,
    pub format: PixelFormat,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ImageLoc {
    pub base: u64,
    pub len: u64,
    /// 0 = A, 1 = B.
    pub slot: u32,
    pub _pad: u32,
    pub version: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct KernelLoc {
    pub phys: u64,
    pub virt: u64,
    pub len: u64,
}

/// Raw TSC readings: B0 entry, B1 image read, B2 verified, B3 ExitBootServices.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BootStamps {
    pub b0: u64,
    pub b1: u64,
    pub b2: u64,
    pub b3: u64,
}

pub const BOOTINFO_MAGIC: u64 = u64::from_le_bytes(*b"FRLYBOOT");
pub const BOOTINFO_VERSION: u32 = 1;

/// Handed from the bootloader to the kernel in RDI (PRD § 3).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BootInfo {
    pub magic: u64,
    pub version: u32,
    pub flavour: u32,
    pub memory_map: Slice<MemRegion>,
    pub framebuffer: Framebuffer,
    pub edid: Slice<u8>,
    pub acpi_rsdp: u64,
    pub uefi_runtime: u64,
    pub image: ImageLoc,
    pub kernel: KernelLoc,
    pub tsc: BootStamps,
    pub cmdline: [u8; 256],
}

/// Error codes (negative in `rax`).
#[repr(i64)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FErr {
    BadHandle = -1,
    WrongType = -2,
    AccessDenied = -3,
    InvalidArgs = -4,
    NoMemory = -5,
    TimedOut = -6,
    PeerClosed = -7,
    ShouldWait = -8,
    BufferTooSmall = -9,
    NotFound = -10,
    AlreadyExists = -11,
    NotSupported = -12,
    Busy = -13,
    BadState = -14,
    /// A signature or hash failed.
    Integrity = -15,
    Internal = -99,
}

/// Rights on a handle (bit flags).
pub mod rights {
    pub const DUPLICATE: u32 = 1 << 0;
    pub const TRANSFER: u32 = 1 << 1;
    pub const READ: u32 = 1 << 2;
    pub const WRITE: u32 = 1 << 3;
    pub const MAP: u32 = 1 << 4;
    pub const EXECUTE: u32 = 1 << 5;
    pub const WAIT: u32 = 1 << 6;
    pub const SIGNAL: u32 = 1 << 7;
    pub const MANAGE: u32 = 1 << 8;
}

/// System call numbers, Freally Core v0.4 (PRD § 4). Number in `rax`.
pub mod syscall {
    /// Dev flavour only: compiled out of release.
    pub const DEBUG_LOG: u64 = 0;
    pub const PROCESS_CREATE: u64 = 1;
    pub const PROCESS_START: u64 = 2;
    pub const PROCESS_EXIT: u64 = 3;
    pub const PROCESS_KILL: u64 = 4;
    pub const PROCESS_INFO: u64 = 5;
    pub const THREAD_CREATE: u64 = 6;
    pub const THREAD_START: u64 = 7;
    pub const THREAD_EXIT: u64 = 8;
    pub const THREAD_SET_CLASS: u64 = 9;
    pub const VMO_CREATE: u64 = 10;
    pub const VMO_READ: u64 = 11;
    pub const VMO_WRITE: u64 = 12;
    pub const VMO_PHYS: u64 = 13;
    pub const VM_MAP: u64 = 14;
    pub const VM_UNMAP: u64 = 15;
    pub const VM_PROTECT: u64 = 16;
    pub const CHANNEL_CREATE: u64 = 17;
    pub const CHANNEL_WRITE: u64 = 18;
    pub const CHANNEL_READ: u64 = 19;
    pub const CHANNEL_CALL: u64 = 20;
    pub const OBJECT_WAIT_ONE: u64 = 21;
    pub const OBJECT_WAIT_MANY: u64 = 22;
    pub const OBJECT_SIGNAL: u64 = 23;
    pub const EVENT_CREATE: u64 = 24;
    pub const HANDLE_CLOSE: u64 = 25;
    pub const HANDLE_DUPLICATE: u64 = 26;
    pub const INTERRUPT_BIND: u64 = 27;
    pub const INTERRUPT_ACK: u64 = 28;
    pub const MMIO_MAP: u64 = 29;
    pub const IOPORT_GRANT: u64 = 30;
    pub const CLOCK_GET: u64 = 31;
    pub const FUTEX_WAIT: u64 = 32;
    pub const FUTEX_WAKE: u64 = 33;
    pub const SYSTEM_INFO: u64 = 34;
    pub const UEFI_SET_VAR: u64 = 35;
    /// One past the highest number.
    pub const COUNT: u64 = 36;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootinfo_magic_spells_frlyboot() {
        assert_eq!(&BOOTINFO_MAGIC.to_le_bytes(), b"FRLYBOOT");
    }

    #[test]
    fn guid_is_sixteen_bytes() {
        assert_eq!(core::mem::size_of::<Guid>(), 16);
    }

    #[test]
    fn flavours_round_trip() {
        for f in [Flavour::Release, Flavour::Dev, Flavour::E2e] {
            assert_eq!(Flavour::from_u32(f as u32), Some(f));
        }
        assert_eq!(Flavour::from_u32(3), None);
    }
}

/// The kernel's address-space layout (TASK-018).
pub mod layout {
    /// All physical memory is mapped here, read/write, never executable.
    pub const DIRECT_MAP: u64 = 0xFFFF_8000_0000_0000;
    /// Freally Core is linked here (the top 2 GiB, `-mcmodel=kernel`).
    pub const KERNEL_BASE: u64 = 0xFFFF_FFFF_8000_0000;
    /// The kernel's boot stack (in the direct map), in bytes.
    pub const BOOT_STACK: u64 = 64 * 1024;
    /// User programs are linked here (`--image-base`).
    pub const USER_IMAGE_BASE: u64 = 0x40_0000;
    /// The top of a program's first stack; the page under its bottom stays unmapped.
    pub const USER_STACK_TOP: u64 = 0x7FFF_FFF0_0000;
    /// The size of a program's first stack, in bytes.
    pub const USER_STACK: u64 = 256 * 1024;
    /// The last-panic page (physical): a kernel panic writes it, the next boot reads it.
    pub const LAST_PANIC_PHYS: u64 = 0x0400_0000;
}

/// UEFI memory types the bootloader allocates with (the OS-loader range,
/// 0x8000_0000 and up), so the kernel can tell its own memory from free memory.
pub mod memtype {
    /// Freally Core's loaded segments.
    pub const KERNEL: u32 = 0x8000_0001;
    /// BootInfo, the memory map copy, the page tables and the boot stack.
    pub const BOOT_DATA: u32 = 0x8000_0002;
    /// The verified system image, kept for conductor.
    pub const IMAGE: u32 = 0x8000_0003;
}

/// Signal bits on objects; user space may set and clear only `USER_MASK`.
pub mod signal {
    pub const READABLE: u32 = 1 << 0;
    pub const WRITABLE: u32 = 1 << 1;
    pub const PEER_CLOSED: u32 = 1 << 2;
    pub const TERMINATED: u32 = 1 << 3;
    pub const SIGNALED: u32 = 1 << 4;
    pub const USER_MASK: u32 = 0xFF00_0000;
}

/// Handles every process is started with.
pub mod well_known {
    /// The channel the process was started with (to conductor, for services).
    pub const BOOTSTRAP: u32 = 1;
    /// The process itself (READ, MANAGE over its own threads).
    pub const SELF_PROCESS: u32 = 2;
    /// The process's own address space (MAP).
    pub const SELF_ASPACE: u32 = 3;
    /// Conductor only: the root resource (every hardware resource derives from it).
    pub const ROOT_RESOURCE: u32 = 4;
    /// Conductor only: the verified system image, read-only.
    pub const IMAGE_VMO: u32 = 5;
    /// Conductor only: the BootInfo page, read-only.
    pub const BOOTINFO_VMO: u32 = 6;
    /// Conductor only: the event the kernel signals (bit 24) when it files a fault report.
    pub const FAULTS_EVENT: u32 = 7;
}

/// A user panic ends in `ud2` with RAX = this, RDI = the message, RSI = its
/// length: the kernel files a `Panic` fault report with that message.
pub const USER_PANIC_MAGIC: u64 = u64::from_le_bytes(*b"FPANIC!!");

/// `clock_get` clocks.
pub mod clock {
    pub const MONOTONIC: u64 = 0;
    pub const BOOT: u64 = 1;
    pub const UTC: u64 = 2;
}

/// `system_info` topics.
pub mod topic {
    pub const CPU: u64 = 0;
    pub const MEMORY: u64 = 1;
    pub const BOOT_TIMELINE: u64 = 2;
    pub const FIRMWARE_TIMES: u64 = 3;
    pub const KERNEL_LOG: u64 = 4;
    pub const FAULTS: u64 = 5;
}

/// `vm_map` / `vm_protect` permissions.
pub mod perm {
    pub const READ: u64 = 1;
    pub const WRITE: u64 = 2;
    pub const EXEC: u64 = 4;
}

/// Grants: what a process may ask the kernel for beyond its own objects.
pub mod grant {
    pub const PROCESS_CREATE: u32 = 1 << 0;
    pub const REALTIME: u32 = 1 << 1;
    pub const DMA: u32 = 1 << 2;
    pub const FAULTS: u32 = 1 << 3;
}

impl FErr {
    /// The error a negative system-call result stands for.
    pub fn from_code(v: i64) -> FErr {
        match v {
            -1 => FErr::BadHandle,
            -2 => FErr::WrongType,
            -3 => FErr::AccessDenied,
            -4 => FErr::InvalidArgs,
            -5 => FErr::NoMemory,
            -6 => FErr::TimedOut,
            -7 => FErr::PeerClosed,
            -8 => FErr::ShouldWait,
            -9 => FErr::BufferTooSmall,
            -10 => FErr::NotFound,
            -11 => FErr::AlreadyExists,
            -12 => FErr::NotSupported,
            -13 => FErr::Busy,
            -14 => FErr::BadState,
            -15 => FErr::Integrity,
            _ => FErr::Internal,
        }
    }

    /// A short, plain description.
    pub fn describe(self) -> &'static str {
        match self {
            FErr::BadHandle => "a handle that doesn't exist",
            FErr::WrongType => "the wrong kind of object",
            FErr::AccessDenied => "not allowed",
            FErr::InvalidArgs => "invalid arguments",
            FErr::NoMemory => "out of memory",
            FErr::TimedOut => "timed out",
            FErr::PeerClosed => "the other end closed",
            FErr::ShouldWait => "nothing yet; wait",
            FErr::BufferTooSmall => "the buffer is too small",
            FErr::NotFound => "not found",
            FErr::AlreadyExists => "already exists",
            FErr::NotSupported => "not supported",
            FErr::Busy => "busy",
            FErr::BadState => "not in a state to do that",
            FErr::Integrity => "a signature or hash didn't match",
            FErr::Internal => "an internal error",
        }
    }
}

impl From<FErr> for &'static str {
    fn from(e: FErr) -> &'static str {
        e.describe()
    }
}
