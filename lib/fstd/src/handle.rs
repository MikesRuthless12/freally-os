// SPDX-License-Identifier: Apache-2.0
//! Owned handles: closed when dropped.

use crate::sys;
use fcore_abi::FErr;

#[derive(Debug, PartialEq, Eq)]
pub struct Handle(u32);

impl Handle {
    /// Takes ownership of a raw handle value (0 is never a handle).
    pub fn from_raw(v: u32) -> Option<Handle> {
        (v != 0).then_some(Handle(v))
    }

    pub fn raw(&self) -> u32 {
        self.0
    }

    /// Gives up ownership without closing (the handle is being moved in a message).
    pub fn into_raw(self) -> u32 {
        let v = self.0;
        core::mem::forget(self);
        v
    }

    /// A second handle to the same object, with `rights` (only ever narrower).
    pub fn duplicate(&self, rights: u32) -> Result<Handle, FErr> {
        let v = sys::call1(fcore_abi::syscall::HANDLE_DUPLICATE, [self.0 as u64, rights as u64, 0, 0, 0, 0])?;
        Ok(Handle(v as u32))
    }

    pub fn wait(&self, signals: u32, deadline_ns: u64) -> Result<u32, FErr> {
        sys::wait_one(self.0, signals, deadline_ns)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = sys::handle_close(self.0);
    }
}
