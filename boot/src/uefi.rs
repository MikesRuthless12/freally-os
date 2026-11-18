// SPDX-License-Identifier: Apache-2.0
//! Freally's own UEFI bindings, written from the UEFI 2.10 specification. Only
//! what the bootloader uses is typed; every other table slot is a placeholder
//! of the right size, so the offsets match the specification's layout.

use core::ffi::c_void;
use fcore_abi::Guid;

pub type Handle = *mut c_void;
pub type Status = usize;

const ERROR_BIT: usize = 1 << 63;
pub const BUFFER_TOO_SMALL: Status = ERROR_BIT | 5;

pub fn is_error(s: Status) -> bool {
    s & ERROR_BIT != 0
}

/// A placeholder for a function pointer this bootloader never calls.
type Unused = usize;

#[repr(C)]
pub struct TableHeader {
    pub signature: u64,
    pub revision: u32,
    pub header_size: u32,
    pub crc32: u32,
    pub reserved: u32,
}

#[repr(C)]
pub struct SystemTable {
    pub hdr: TableHeader,
    pub firmware_vendor: *const u16,
    pub firmware_revision: u32,
    pub console_in_handle: Handle,
    pub con_in: *mut c_void,
    pub console_out_handle: Handle,
    pub con_out: *mut SimpleTextOutput,
    pub standard_error_handle: Handle,
    pub std_err: *mut SimpleTextOutput,
    pub runtime_services: *mut RuntimeServices,
    pub boot_services: *mut BootServices,
    pub number_of_table_entries: usize,
    pub configuration_table: *mut ConfigurationTable,
}

#[repr(C)]
pub struct ConfigurationTable {
    pub vendor_guid: Guid,
    pub vendor_table: *mut c_void,
}

#[repr(C)]
pub struct SimpleTextOutput {
    pub reset: extern "efiapi" fn(*mut SimpleTextOutput, u8) -> Status,
    pub output_string: extern "efiapi" fn(*mut SimpleTextOutput, *const u16) -> Status,
    pub test_string: Unused,
    pub query_mode: Unused,
    pub set_mode: Unused,
    pub set_attribute: Unused,
    pub clear_screen: extern "efiapi" fn(*mut SimpleTextOutput) -> Status,
    pub set_cursor_position: Unused,
    pub enable_cursor: extern "efiapi" fn(*mut SimpleTextOutput, u8) -> Status,
    pub mode: *mut c_void,
}

#[repr(C)]
pub struct BootServices {
    pub hdr: TableHeader,
    // Task priority
    pub raise_tpl: Unused,
    pub restore_tpl: Unused,
    // Memory
    pub allocate_pages: extern "efiapi" fn(u32, u32, usize, *mut u64) -> Status,
    pub free_pages: extern "efiapi" fn(u64, usize) -> Status,
    pub get_memory_map: extern "efiapi" fn(*mut usize, *mut u8, *mut usize, *mut usize, *mut u32) -> Status,
    pub allocate_pool: extern "efiapi" fn(u32, usize, *mut *mut u8) -> Status,
    pub free_pool: extern "efiapi" fn(*mut u8) -> Status,
    // Events and timers
    pub create_event: Unused,
    pub set_timer: Unused,
    pub wait_for_event: Unused,
    pub signal_event: Unused,
    pub close_event: Unused,
    pub check_event: Unused,
    // Protocol handlers
    pub install_protocol_interface: Unused,
    pub reinstall_protocol_interface: Unused,
    pub uninstall_protocol_interface: Unused,
    pub handle_protocol: extern "efiapi" fn(Handle, *const Guid, *mut *mut c_void) -> Status,
    pub reserved: Unused,
    pub register_protocol_notify: Unused,
    pub locate_handle: Unused,
    pub locate_device_path: Unused,
    pub install_configuration_table: Unused,
    // Images
    pub load_image: Unused,
    pub start_image: Unused,
    pub exit: Unused,
    pub unload_image: Unused,
    pub exit_boot_services: extern "efiapi" fn(Handle, usize) -> Status,
    // Miscellaneous
    pub get_next_monotonic_count: Unused,
    pub stall: extern "efiapi" fn(usize) -> Status,
    pub set_watchdog_timer: extern "efiapi" fn(usize, u64, usize, *const u16) -> Status,
    // Driver support
    pub connect_controller: Unused,
    pub disconnect_controller: Unused,
    // Open and close protocol
    pub open_protocol: Unused,
    pub close_protocol: Unused,
    pub open_protocol_information: Unused,
    // Library
    pub protocols_per_handle: Unused,
    pub locate_handle_buffer: Unused,
    pub locate_protocol: extern "efiapi" fn(*const Guid, *mut c_void, *mut *mut c_void) -> Status,
    pub install_multiple_protocol_interfaces: Unused,
    pub uninstall_multiple_protocol_interfaces: Unused,
    // 32-bit CRC
    pub calculate_crc32: Unused,
    // Miscellaneous
    pub copy_mem: Unused,
    pub set_mem: Unused,
    pub create_event_ex: Unused,
}

#[repr(C)]
pub struct RuntimeServices {
    pub hdr: TableHeader,
    pub get_time: Unused,
    pub set_time: Unused,
    pub get_wakeup_time: Unused,
    pub set_wakeup_time: Unused,
    pub set_virtual_address_map: extern "efiapi" fn(usize, usize, u32, *mut u8) -> Status,
    pub convert_pointer: Unused,
    pub get_variable: extern "efiapi" fn(*const u16, *const Guid, *mut u32, *mut usize, *mut c_void) -> Status,
    pub get_next_variable_name: Unused,
    pub set_variable: extern "efiapi" fn(*const u16, *const Guid, u32, usize, *const c_void) -> Status,
    pub get_next_high_monotonic_count: Unused,
    pub reset_system: Unused,
    pub update_capsule: Unused,
    pub query_capsule_capabilities: Unused,
    pub query_variable_info: Unused,
}

/// Memory types (EFI_MEMORY_TYPE).
pub const LOADER_DATA: u32 = 2;
/// Allocate types (EFI_ALLOCATE_TYPE).
pub const ALLOCATE_ANY_PAGES: u32 = 0;

#[repr(C)]
pub struct LoadedImage {
    pub revision: u32,
    pub parent_handle: Handle,
    pub system_table: *mut SystemTable,
    pub device_handle: Handle,
    pub file_path: *mut c_void,
    pub reserved: *mut c_void,
    pub load_options_size: u32,
    pub load_options: *mut c_void,
    pub image_base: *mut c_void,
    pub image_size: u64,
    pub image_code_type: u32,
    pub image_data_type: u32,
    pub unload: Unused,
}

pub const LOADED_IMAGE_GUID: Guid =
    Guid::new(0x5B1B_31A1, 0x9562, 0x11D2, [0x8E, 0x3F, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B]);

#[repr(C)]
pub struct SimpleFileSystem {
    pub revision: u64,
    pub open_volume: extern "efiapi" fn(*mut SimpleFileSystem, *mut *mut File) -> Status,
}

pub const SIMPLE_FILE_SYSTEM_GUID: Guid =
    Guid::new(0x964E_5B22, 0x6459, 0x11D2, [0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B]);

#[repr(C)]
pub struct File {
    pub revision: u64,
    pub open: extern "efiapi" fn(*mut File, *mut *mut File, *const u16, u64, u64) -> Status,
    pub close: extern "efiapi" fn(*mut File) -> Status,
    pub delete: Unused,
    pub read: extern "efiapi" fn(*mut File, *mut usize, *mut u8) -> Status,
    pub write: Unused,
    pub get_position: extern "efiapi" fn(*mut File, *mut u64) -> Status,
    pub set_position: extern "efiapi" fn(*mut File, u64) -> Status,
    pub get_info: Unused,
    pub set_info: Unused,
    pub flush: Unused,
}

pub const FILE_MODE_READ: u64 = 1;

#[repr(C)]
pub struct GraphicsOutput {
    pub query_mode: Unused,
    pub set_mode: Unused,
    pub blt: extern "efiapi" fn(
        *mut GraphicsOutput,
        *mut u32,
        u32,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
    ) -> Status,
    pub mode: *mut GraphicsMode,
}

#[repr(C)]
pub struct GraphicsMode {
    pub max_mode: u32,
    pub mode: u32,
    pub info: *mut GraphicsModeInfo,
    pub size_of_info: usize,
    pub frame_buffer_base: u64,
    pub frame_buffer_size: usize,
}

#[repr(C)]
pub struct GraphicsModeInfo {
    pub version: u32,
    pub horizontal_resolution: u32,
    pub vertical_resolution: u32,
    /// 0 RGB (byte 0 red), 1 BGR (byte 0 blue), 2 bit mask, 3 Blt only.
    pub pixel_format: u32,
    /// Red, green, blue and reserved masks (pixel format 2 only).
    pub pixel_information: [u32; 4],
    pub pixels_per_scan_line: u32,
}

pub const GRAPHICS_OUTPUT_GUID: Guid =
    Guid::new(0x9042_A9DE, 0x23DC, 0x4A38, [0x96, 0xFB, 0x7A, 0xDE, 0xD0, 0x80, 0x51, 0x6A]);
/// EfiBltBufferToVideo.
pub const BLT_BUFFER_TO_VIDEO: u32 = 2;

#[repr(C)]
pub struct EdidActive {
    pub size_of_edid: u32,
    pub edid: *mut u8,
}

pub const EDID_ACTIVE_GUID: Guid =
    Guid::new(0xBD8C_1056, 0x9F36, 0x44EC, [0x92, 0xA8, 0xA6, 0x33, 0x7F, 0x81, 0x79, 0x86]);

/// ACPI 2.0+ RSDP in the configuration table.
pub const ACPI_20_TABLE_GUID: Guid =
    Guid::new(0x8868_E871, 0xE4F1, 0x11D3, [0xBC, 0x22, 0x00, 0x80, 0xC7, 0x3C, 0x88, 0x81]);
