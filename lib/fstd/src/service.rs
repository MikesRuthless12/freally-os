// SPDX-License-Identifier: Apache-2.0
//! A service's side of conductor (TASK-038): the start message, the handles
//! its manifest's grants carry, the directory (`connect`, `register`).
//!
//! Handle 1 is the directory channel. Conductor's first message on it is
//! `start`; `service::start()` reads it before anything else. Services are
//! single-threaded for now, so the state is a plain cell.

use crate::handle::Handle;
use crate::ipc::{CallError, Channel, Client};
use crate::time::{Duration, Instant};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use fcore_abi::{FErr, well_known};
use fwire::HandleRef;
use fwire::proto::conductor::{self as c, ByName, Connected, Register, Stamp, Start};

struct State {
    name: String,
    flavour: u8,
    handles: Vec<(String, Handle)>,
    /// The data files mapped so far, by name: asked for again, the same bytes.
    mapped: Vec<(String, &'static [u8])>,
    directory: Client,
}

struct Global(RefCell<Option<State>>);
// SAFETY: services are single-threaded until fstd has a lock (see above).
unsafe impl Sync for Global {}
static STATE: Global = Global(RefCell::new(None));

/// Reads conductor's `start` message from handle 1.
pub fn start() -> Result<(), CallError> {
    let dir = Channel(Handle::from_raw(well_known::BOOTSTRAP).ok_or(FErr::BadHandle)?);
    let m = dir.recv(Instant::after(Duration::from_secs(5)))?;
    let (h, payload) = fwire::parse(&m.bytes)?;
    if h.ordinal != c::START || !h.is_event() {
        return Err(CallError::Wire(fwire::WireError::BadTag));
    }
    let s: Start = fwire::from_payload(payload)?;
    let mut given: Vec<Option<Handle>> = m.handles.into_iter().map(Some).collect();
    let mut handles = Vec::new();
    for (name, HandleRef(i)) in s.handles {
        if let Some(h) = given.get_mut(i as usize).and_then(Option::take) {
            handles.push((name, h));
        }
    }
    *STATE.0.borrow_mut() =
        Some(State { name: s.name, flavour: s.flavour, handles, mapped: Vec::new(), directory: Client::new(dir) });
    Ok(())
}

/// The service's name, from its manifest.
pub fn name() -> String {
    STATE.0.borrow().as_ref().map(|s| s.name.clone()).unwrap_or_default()
}

/// The image's flavour: 0 release, 1 dev, 2 e2e.
pub fn flavour() -> u8 {
    STATE.0.borrow().as_ref().map_or(0, |s| s.flavour)
}

/// Takes the handle a grant named `name` carried.
pub fn take_handle(name: &str) -> Option<Handle> {
    let mut st = STATE.0.borrow_mut();
    let s = st.as_mut()?;
    let i = s.handles.iter().position(|(n, _)| n == name)?;
    Some(s.handles.remove(i).1)
}

/// A channel to the service called `name`, waiting up to 5 s for it to register.
pub fn connect(name: &str) -> Result<Channel, CallError> {
    connect_within(name, Duration::from_secs(5))
}

/// As `connect`, waiting at most `wait` for the service to register (zero:
/// one try, as the log client does when the logger is restarting).
pub fn connect_within(name: &str, wait: Duration) -> Result<Channel, CallError> {
    let start = Instant::now();
    loop {
        let r = {
            let mut st = STATE.0.borrow_mut();
            let s = st.as_mut().ok_or(CallError::Sys(FErr::BadState))?;
            s.directory.call::<_, Connected>(
                c::CONNECT,
                &ByName { name: String::from(name) },
                Vec::new(),
                Instant::after(Duration::from_secs(2)),
            )
        };
        match r {
            Ok((_, mut handles)) if !handles.is_empty() => return Ok(Channel(handles.remove(0))),
            Ok(_) => return Err(CallError::Sys(FErr::Internal)),
            Err(CallError::Remote(code, _)) if code == FErr::ShouldWait as i32 && start.elapsed() < wait => {
                crate::time::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Registers this service's endpoint: conductor sends each new client to the
/// other end (an `incoming` event with the client's channel).
pub fn register(endpoint: Channel) -> Result<(), CallError> {
    let mut st = STATE.0.borrow_mut();
    let s = st.as_mut().ok_or(CallError::Sys(FErr::BadState))?;
    let name = s.name.clone();
    s.directory.call::<_, ()>(
        c::REGISTER,
        &Register { name, endpoint: HandleRef(0) },
        alloc::vec![endpoint.0],
        Instant::after(Duration::from_secs(2)),
    )?;
    Ok(())
}

/// Marks a boot stage (PRD § 3) at TSC `tsc`: S3 (13) is stage's, S4 (14)
/// the desktop's. Conductor takes each once, from its own service only.
pub fn stamp(stage: u8, tsc: u64) -> Result<(), CallError> {
    let mut st = STATE.0.borrow_mut();
    let s = st.as_mut().ok_or(CallError::Sys(FErr::BadState))?;
    s.directory.call::<_, ()>(c::STAMP, &Stamp { stage, tsc }, Vec::new(), Instant::after(Duration::from_secs(2)))?;
    Ok(())
}

/// The font a `fonts` grant carried as `font:<file>` (FR-026).
pub fn font(file: &str) -> Result<&'static [u8], FErr> {
    data(&alloc::format!("font:{file}"))
}

/// A data file a grant allows (`font:<file>`, handed over at start;
/// `hardware:<pct>`, `os:<pct>`, `os:wallpaper`, asked of conductor the first
/// time, TASK-090A): conductor writes its length (u64, little-endian) and
/// then the file; mapped read-only for as long as the service runs, once.
pub fn data(name: &str) -> Result<&'static [u8], FErr> {
    if let Some(bytes) = STATE.0.borrow().as_ref().and_then(|s| s.mapped.iter().find(|(n, _)| n == name).map(|m| m.1)) {
        return Ok(bytes);
    }
    let h = match take_handle(name) {
        Some(h) => h,
        None => {
            let (_, mut hs) =
                ask_moving::<_, ()>(c::DATA, &ByName { name: String::from(name) }, Vec::new(), Duration::from_secs(5))
                    .map_err(|e| match e {
                        CallError::Sys(e) => e,
                        CallError::Remote(code, _) => FErr::from_code(code as i64),
                        CallError::Wire(_) => FErr::InvalidArgs,
                    })?;
            if hs.is_empty() {
                return Err(FErr::NotFound);
            }
            hs.swap_remove(0)
        }
    };
    let mut len = [0u8; 8];
    crate::sys::vmo_read(h.raw(), 0, &mut len)?;
    let len = u64::from_le_bytes(len);
    let at = crate::sys::vm_map(
        well_known::SELF_ASPACE,
        h.raw(),
        0,
        (len + 8).next_multiple_of(4096),
        0,
        fcore_abi::perm::READ,
    )?;
    // The mapping lives as long as the service: keep the VMO's handle with it.
    core::mem::forget(h);
    // SAFETY: the kernel just mapped len + 8 readable bytes at `at`; they stay mapped.
    let bytes: &'static [u8] = unsafe { core::slice::from_raw_parts((at + 8) as *const u8, len as usize) };
    if let Some(s) = STATE.0.borrow_mut().as_mut() {
        s.mapped.push((String::from(name), bytes));
    }
    Ok(bytes)
}

/// Asks conductor `ordinal` with `req` (a method conductor serves, such as
/// `list_services`), waiting up to 2 s for the answer.
pub fn ask<Req: fwire::Wire, Rep: fwire::Wire>(ordinal: u32, req: &Req) -> Result<Rep, CallError> {
    let mut st = STATE.0.borrow_mut();
    let s = st.as_mut().ok_or(CallError::Sys(FErr::BadState))?;
    Ok(s.directory.call::<_, Rep>(ordinal, req, Vec::new(), Instant::after(Duration::from_secs(2)))?.0)
}

/// As [`ask`], moving `handles` along and waiting up to `wait` (a swap hands
/// conductor the new build: TASK-083).
pub fn ask_moving<Req: fwire::Wire, Rep: fwire::Wire>(
    ordinal: u32,
    req: &Req,
    handles: Vec<Handle>,
    wait: Duration,
) -> Result<(Rep, Vec<Handle>), CallError> {
    let mut st = STATE.0.borrow_mut();
    let s = st.as_mut().ok_or(CallError::Sys(FErr::BadState))?;
    s.directory.call::<_, Rep>(ordinal, req, handles, Instant::after(wait))
}

/// A VMO holding `bytes`' length (u64, little-endian) and then the bytes: how
/// a swap's new build, a handed-over state and a grant's data file travel
/// (TASK-083, TASK-090A). Written 16 MiB at a time, the most one
/// `vmo_write` takes.
pub fn sized_vmo(bytes: &[u8]) -> Result<Handle, FErr> {
    let h = Handle::from_raw(crate::sys::vmo_create(bytes.len() as u64 + 8)?).ok_or(FErr::NoMemory)?;
    crate::sys::vmo_write(h.raw(), 0, &(bytes.len() as u64).to_le_bytes())?;
    for (i, piece) in bytes.chunks(16 << 20).enumerate() {
        crate::sys::vmo_write(h.raw(), 8 + (i as u64) * (16 << 20), piece)?;
    }
    Ok(h)
}

/// Reads a [`sized_vmo`] back; one claiming more than `max` bytes is refused.
pub fn read_sized(h: &Handle, max: u64) -> Result<Vec<u8>, FErr> {
    let mut len = [0u8; 8];
    crate::sys::vmo_read(h.raw(), 0, &mut len)?;
    let len = u64::from_le_bytes(len);
    if len > max {
        return Err(FErr::InvalidArgs);
    }
    let mut out = alloc::vec![0u8; len as usize];
    crate::sys::vmo_read(h.raw(), 8, &mut out)?;
    Ok(out)
}

/// The state the build before this one handed over in a swap (dev images),
/// if there was one (TASK-083, TASK-085).
pub fn handed_over() -> Option<Vec<u8>> {
    let h = take_handle("handoff")?;
    read_sized(&h, 16 << 20).ok()
}

/// Answers conductor's `handoff` request (`txid`, on `ch`) with `state`.
pub fn hand_over(ch: &Channel, txid: u32, state: &[u8]) -> Result<(), FErr> {
    let vmo = sized_vmo(state)?;
    let body = fwire::to_payload(&c::HandedOff { state: HandleRef(0) });
    let m = fwire::message_bytes(txid, c::HANDOFF, fwire::flags::REPLY, body).map_err(|_| FErr::Internal)?;
    ch.send(&m, alloc::vec![vmo])
}
