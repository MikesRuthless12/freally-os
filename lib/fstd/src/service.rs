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
use fwire::proto::conductor::{self as c, ByName, Connected, Register, Start};

struct State {
    name: String,
    flavour: u8,
    handles: Vec<(String, Handle)>,
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
    *STATE.0.borrow_mut() = Some(State { name: s.name, flavour: s.flavour, handles, directory: Client::new(dir) });
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
            Err(CallError::Remote(code, _))
                if code == FErr::ShouldWait as i32 && start.elapsed() < Duration::from_secs(5) =>
            {
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
