// SPDX-License-Identifier: Apache-2.0
//! Swap, handoff and rollback (TASK-083, FR-018): dev images only, the Brain
//! the only caller. Compiled out of release and e2e.
//!
//! - `swap`: the new build's SHA-256 must carry the dev key's signature, and
//!   it must be a program conductor can load, before anything stops. A
//!   service with `handoff = true` hands its state over (2 s, or the new
//!   build starts without it, said in the log); the old instance stops; the
//!   new build starts with the state as its `handoff` grant and registers
//!   the name again.
//! - The build before is kept: `rollback` puts it back, and a new build that
//!   ends within 5 s is rolled back by itself (`Conductor::exited`).
//! - `restart`: the service stopped and started again at once, its build
//!   unchanged, never counted as a crash.
//!
//! (Compiled into host tests too, for [`check_build`]'s.)
#![cfg_attr(not(feature = "flavour-dev"), allow(dead_code))]

use crate::services::Conductor;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::FErr;
use fstd::handle::Handle;
use fstd::ipc::{CallError, Client};
use fstd::time::{Duration, Instant};
use fwire::proto::conductor::{self as c, ServiceState};

/// The public keys conductor shares with the bootloader (`cargo xtask keygen`
/// writes them); a swap trusts the dev key alone.
#[allow(dead_code)]
#[path = "../../../boot/src/keys.rs"]
mod keys;

/// How long a new build must keep running before a crash is its own.
const PROBATION_NS: u64 = 5_000_000_000;
/// The largest build a swap takes.
const MAX_BUILD: u64 = 64 << 20;

fn only_brain(caller: &str) -> Result<(), (FErr, String)> {
    if caller == "brain" { Ok(()) } else { Err((FErr::AccessDenied, String::from("only the Brain swaps services"))) }
}

/// Checks a new build: the dev key's signature over its SHA-256, and an ELF
/// conductor can load. Its SHA-256 on success.
pub fn check_build(build: &[u8], signature: &[u8; 64], dev_key: &[u8; 32]) -> Result<[u8; 32], (FErr, String)> {
    let sha = fcrypto::sha2::sha256(build);
    if !fcrypto::ed25519::verify(dev_key, &sha, signature) {
        return Err((FErr::Integrity, String::from("the build isn't signed with the dev key; nothing changed")));
    }
    use fcore_abi::layout::{USER_IMAGE_BASE, USER_STACK, USER_STACK_TOP};
    felf::Elf::parse(build, USER_IMAGE_BASE, USER_STACK_TOP - USER_STACK - 4096)
        .map_err(|e| (FErr::InvalidArgs, alloc::format!("not a program conductor can load: {}", e.describe())))?;
    Ok(sha)
}

impl Conductor {
    fn index(&self, name: &str) -> Result<usize, (FErr, String)> {
        self.services
            .iter()
            .position(|s| s.m.name == name)
            .ok_or((FErr::NotFound, alloc::format!("no service is called {name}")))
    }

    /// The running instance's state, if its manifest says it hands it over:
    /// asked on a connection of conductor's own, conductor serving every
    /// other request meanwhile (the service may ask it things as it packs).
    fn take_state(&mut self, i: usize) -> Option<Handle> {
        let name = self.services[i].m.name.clone();
        if !self.services[i].m.handoff {
            return None;
        }
        let mut cl = Client::new(self.open_connection(&name, "conductor")?);
        let (who, dirs): (Vec<usize>, Vec<u32>) = self
            .services
            .iter()
            .enumerate()
            .filter_map(|(k, s)| s.child.as_ref().map(|ch| (k, ch.channel.0.raw())))
            .unzip();
        let deadline = Instant::after(Duration::from_secs(2));
        match cl.call_serving::<_, c::HandedOff>(c::HANDOFF, &(), deadline, &dirs, |k| self.request(who[k])) {
            Ok((_, mut hs)) if !hs.is_empty() => Some(hs.remove(0)),
            Ok(_) => None,
            Err(CallError::Sys(FErr::TimedOut)) => {
                fstd::warn!("conductor: {name} didn't hand its state over in 2 s; the new build starts without it");
                None
            }
            Err(e) => {
                fstd::warn!("conductor: {name} couldn't hand its state over ({e:?}); the new build starts without it");
                None
            }
        }
    }

    /// Stops service `i` (its state taken first) and starts `build` (None:
    /// the image's) in its place at once.
    fn replace(&mut self, i: usize, build: Option<Rc<[u8]>>) {
        let state = self.take_state(i);
        let s = &mut self.services[i];
        if let Some(child) = s.child.take() {
            let _ = fstd::sys::process_kill(child.process.raw());
        }
        s.endpoint = None;
        s.build = build;
        s.handoff = state;
        s.state = ServiceState::Restarting;
        s.restarts += 1;
        s.start_at = Some(Instant::now().as_ns());
        self.start_due();
    }

    /// `swap { name, binary, signature }`.
    pub fn swap(
        &mut self,
        caller: &str,
        payload: &[u8],
        handles: &mut [Option<Handle>],
    ) -> Result<(Vec<u8>, Vec<Handle>), (FErr, String)> {
        only_brain(caller)?;
        let t0 = Instant::now().as_ns();
        let sw: c::Swap = fwire::from_payload(payload).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))?;
        let vmo = handles
            .get_mut(sw.binary.0 as usize)
            .and_then(Option::take)
            .ok_or((FErr::InvalidArgs, String::from("no build")))?;
        let build = fstd::service::read_sized(&vmo, MAX_BUILD)
            .map_err(|e| (e, String::from("the build couldn't be read (at most 64 MiB)")))?;
        let i = self.index(&sw.name)?;
        if sw.name == "brain" {
            return Err((FErr::BadState, String::from("the Brain can't swap itself while it's answering")));
        }
        let new_sha = check_build(&build, &sw.signature, &keys::DEV)?;
        let old_sha = self.services[i].sha;
        let before = self.services[i].build.clone();
        self.replace(i, Some(Rc::from(build)));
        let s = &mut self.services[i];
        s.before = Some(before);
        s.probation = Some(Instant::now().as_ns() + PROBATION_NS);
        let ms = (Instant::now().as_ns().saturating_sub(t0) / 1_000_000) as u32;
        fstd::info!("conductor: {} swapped in {ms} ms", sw.name);
        Ok((fwire::to_payload(&c::Swapped { old_sha256: old_sha, new_sha256: new_sha, ms }), Vec::new()))
    }

    /// `rollback { name }`: the build before the last swap, back.
    pub fn rollback(&mut self, caller: &str, payload: &[u8]) -> Result<(Vec<u8>, Vec<Handle>), (FErr, String)> {
        only_brain(caller)?;
        let b: c::ByName = fwire::from_payload(payload).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))?;
        let i = self.index(&b.name)?;
        let before = self.services[i]
            .before
            .take()
            .ok_or((FErr::BadState, alloc::format!("{} hasn't been swapped since it last rolled back", b.name)))?;
        self.services[i].probation = None;
        self.replace(i, before);
        let restored = self.services[i].sha;
        fstd::info!("conductor: {} rolled back", b.name);
        Ok((fwire::to_payload(&c::RolledBack { restored_sha256: restored }), Vec::new()))
    }

    /// `restart { name }`: stopped and started again, its build unchanged.
    pub fn restart_now(&mut self, caller: &str, payload: &[u8]) -> Result<(Vec<u8>, Vec<Handle>), (FErr, String)> {
        only_brain(caller)?;
        let b: c::ByName = fwire::from_payload(payload).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))?;
        let i = self.index(&b.name)?;
        if b.name == "brain" {
            return Err((FErr::BadState, String::from("the Brain can't restart itself while it's answering")));
        }
        // A restart hands nothing over: it's the same build, started fresh.
        let s = &mut self.services[i];
        if let Some(child) = s.child.take() {
            let _ = fstd::sys::process_kill(child.process.raw());
        }
        s.endpoint = None;
        s.state = ServiceState::Restarting;
        s.restarts += 1;
        s.start_at = Some(Instant::now().as_ns());
        self.start_due();
        fstd::info!("conductor: {} restarted (the Brain asked)", b.name);
        Ok((Vec::new(), Vec::new()))
    }
}

impl Conductor {
    /// `reboot { other_slot }`: answered, then the machine resets half a
    /// second later, so the Brain's answer leaves over COM2 first.
    pub fn reboot(&mut self, caller: &str, payload: &[u8]) -> Result<(Vec<u8>, Vec<Handle>), (FErr, String)> {
        only_brain(caller)?;
        let r: c::Reboot = fwire::from_payload(payload).map_err(|e| (FErr::InvalidArgs, String::from(e.describe())))?;
        if r.other_slot {
            self.boot.switch_to_other().map_err(|why| (FErr::BadState, why))?;
        }
        self.reboot_at = Some(Instant::now().as_ns() + 500_000_000);
        fstd::warn!(
            "conductor: the machine restarts in half a second{} (the Brain asked)",
            if r.other_slot { ", into the other slot" } else { "" }
        );
        Ok((Vec::new(), Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::check_build;
    use fcore_abi::FErr;
    use fcore_abi::layout::USER_IMAGE_BASE;

    /// A one-segment program (read and execute) at the user image base.
    fn program() -> alloc::vec::Vec<u8> {
        let mut b = alloc::vec![0u8; 64 + 56];
        b[0..4].copy_from_slice(b"\x7FELF");
        (b[4], b[5], b[6]) = (2, 1, 1);
        b[16..18].copy_from_slice(&2u16.to_le_bytes());
        b[18..20].copy_from_slice(&0x3Eu16.to_le_bytes());
        b[24..32].copy_from_slice(&(USER_IMAGE_BASE + 0x1000).to_le_bytes());
        b[32..40].copy_from_slice(&64u64.to_le_bytes());
        b[54..56].copy_from_slice(&56u16.to_le_bytes());
        b[56..58].copy_from_slice(&1u16.to_le_bytes());
        let h = 64;
        b[h..h + 4].copy_from_slice(&1u32.to_le_bytes());
        b[h + 4..h + 8].copy_from_slice(&5u32.to_le_bytes());
        b[h + 8..h + 16].copy_from_slice(&120u64.to_le_bytes());
        b[h + 16..h + 24].copy_from_slice(&(USER_IMAGE_BASE + 0x1000).to_le_bytes());
        b[h + 32..h + 40].copy_from_slice(&16u64.to_le_bytes());
        b[h + 40..h + 48].copy_from_slice(&16u64.to_le_bytes());
        b.extend_from_slice(&[0x90; 16]);
        b
    }

    #[test]
    fn only_a_dev_signed_loadable_build_is_taken() {
        // A throwaway key stands in for the dev key.
        let seed = [7u8; 32];
        let key = fcrypto::ed25519::public_key(&seed);
        let build = program();
        let sha = fcrypto::sha2::sha256(&build);
        let sig = fcrypto::ed25519::sign(&seed, &sha);
        assert_eq!(check_build(&build, &sig, &key), Ok(sha));
        // Another key's signature, or one byte of the build changed.
        let other = fcrypto::ed25519::sign(&[8u8; 32], &sha);
        assert_eq!(check_build(&build, &other, &key).unwrap_err().0, FErr::Integrity);
        let mut changed = build.clone();
        changed[130] ^= 1;
        assert_eq!(check_build(&changed, &sig, &key).unwrap_err().0, FErr::Integrity);
        // Signed, but not a program.
        let junk = alloc::vec![0x55u8; 200];
        let junk_sig = fcrypto::ed25519::sign(&seed, &fcrypto::sha2::sha256(&junk));
        assert_eq!(check_build(&junk, &junk_sig, &key).unwrap_err().0, FErr::InvalidArgs);
    }
}
