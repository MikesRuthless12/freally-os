// SPDX-License-Identifier: Apache-2.0
//! Starting a service (TASK-038): its ELF (checked against the TOC) loaded
//! segment by segment into fresh VMOs, each mapped at its linked address with
//! its own permissions (never W and X together, which felf refuses), a stack
//! under a guard page, the manifest's kernel grants, and the `start` message
//! on its directory channel carrying its granted handles.

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::layout::{USER_IMAGE_BASE, USER_STACK, USER_STACK_TOP};
use fcore_abi::{FErr, grant, perm};
use fstd::handle::Handle;
use fstd::ipc::Channel;
use fwire::HandleRef;
use fwire::proto::conductor::{START, Start};

/// The kernel grant a manifest grant names (handle grants carry no bit).
pub fn kernel_grant(name: &str) -> u32 {
    match name {
        "process-create" => grant::PROCESS_CREATE,
        "realtime" => grant::REALTIME,
        "dma" => grant::DMA,
        "faults" => grant::FAULTS,
        "kernel-log" => grant::KERNEL_LOG,
        "boot-control" => grant::BOOT_CONTROL,
        _ => 0,
    }
}

pub struct Child {
    pub process: Handle,
    pub pid: u32,
    /// Conductor's end of the service's directory channel.
    pub channel: Channel,
}

fn err(what: &str, e: FErr) -> String {
    alloc::format!("{what}: {}", e.describe())
}

/// Loads and starts `elf` as `name` with `grants` and the named `handles`.
pub fn spawn(
    elf_bytes: &[u8],
    name: &str,
    grants: u32,
    flavour: u8,
    handles: Vec<(String, Handle)>,
) -> Result<Child, String> {
    let elf = felf::Elf::parse(elf_bytes, USER_IMAGE_BASE, USER_STACK_TOP - USER_STACK - 4096)
        .map_err(|e| String::from(e.describe()))?;
    let (process, aspace) = fstd::sys::process_create(name, grants).map_err(|e| err("process_create", e))?;
    let process = Handle::from_raw(process).ok_or("no process handle")?;
    let aspace = Handle::from_raw(aspace).ok_or("no address-space handle")?;
    for s in elf.segments() {
        let (start, end) = s.pages();
        let vmo =
            Handle::from_raw(fstd::sys::vmo_create(end - start).map_err(|e| err("vmo_create", e))?).ok_or("no VMO")?;
        fstd::sys::vmo_write(vmo.raw(), s.vaddr - start, elf.data(s)).map_err(|e| err("vmo_write", e))?;
        let mut p = perm::READ;
        if s.writable() {
            p |= perm::WRITE;
        }
        if s.executable() {
            p |= perm::EXEC;
        }
        let at = fstd::sys::vm_map(aspace.raw(), vmo.raw(), 0, end - start, start, p).map_err(|e| err("vm_map", e))?;
        if at != start {
            return Err(String::from("a segment didn't land at its linked address"));
        }
    }
    let stack =
        Handle::from_raw(fstd::sys::vmo_create(USER_STACK).map_err(|e| err("vmo_create", e))?).ok_or("no VMO")?;
    let base = USER_STACK_TOP - USER_STACK;
    fstd::sys::vm_map(aspace.raw(), stack.raw(), 0, USER_STACK, base, perm::READ | perm::WRITE)
        .map_err(|e| err("vm_map (stack)", e))?;
    let thread = Handle::from_raw(fstd::sys::thread_create(process.raw(), name).map_err(|e| err("thread_create", e))?)
        .ok_or("no thread")?;
    let (ours, theirs) = Channel::pair().map_err(|e| err("channel_create", e))?;
    let refs: Vec<(String, HandleRef)> =
        handles.iter().enumerate().map(|(i, (n, _))| (n.clone(), HandleRef(i as u8))).collect();
    let start = fwire::event(START, &Start { name: String::from(name), flavour, handles: refs })
        .map_err(|e| String::from(e.describe()))?;
    ours.send(&start, handles.into_iter().map(|(_, h)| h).collect()).map_err(|e| err("the start message", e))?;
    let (pid, ..) = fstd::sys::process_info(process.raw()).map_err(|e| err("process_info", e))?;
    fstd::sys::process_start(process.raw(), thread.raw(), elf.entry, USER_STACK_TOP, theirs.0.into_raw())
        .map_err(|e| err("process_start", e))?;
    Ok(Child { process, pid, channel: ours })
}
