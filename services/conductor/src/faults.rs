// SPDX-License-Identifier: Apache-2.0
//! Fault reports (TASK-031): the kernel files them and signals conductor's
//! faults event; conductor reads them, keeps the newest 32, and logs each new
//! one. (Restarting the service per its manifest is TASK-038.)

use alloc::collections::VecDeque;
use fcore_abi::{FErr, topic, well_known};
use fwire::proto::conductor::{FaultReport, Faults};

pub const KEEP: usize = 32;
/// The faults event's bit (the kernel sets it; conductor clears it).
pub const NEW_FAULT: u32 = 1 << 24;

pub struct Kept {
    pub reports: VecDeque<FaultReport>,
    last_seq: u64,
}

impl Kept {
    pub fn new() -> Kept {
        Kept { reports: VecDeque::new(), last_seq: 0 }
    }

    /// Takes the reports in a `system_info(FAULTS)` payload that are newer than
    /// any kept; returns the new ones still kept. A payload that doesn't decode
    /// is refused rather than trusted.
    pub fn take(&mut self, payload: &[u8]) -> Result<alloc::vec::Vec<&FaultReport>, fwire::WireError> {
        let all: Faults = fwire::from_payload(payload)?;
        let mut added = 0;
        let since = self.last_seq;
        for r in all.reports.into_iter().filter(|r| r.seq > since) {
            self.last_seq = r.seq;
            if self.reports.len() == KEEP {
                self.reports.pop_front();
            }
            self.reports.push_back(r);
            added += 1;
        }
        // More new reports than the ring keeps: only the newest 32 are left to show.
        let kept_new = added.min(self.reports.len());
        Ok(self.reports.iter().skip(self.reports.len() - kept_new).collect())
    }

    /// The kernel signalled: clear the signal, then take and log what's new.
    pub fn take_now(&mut self) -> Result<(), FErr> {
        fstd::sys::object_signal(well_known::FAULTS_EVENT, NEW_FAULT, 0)?;
        let mut buf = alloc::vec![0u8; 64 * 1024];
        let n = fstd::sys::system_info(topic::FAULTS, &mut buf)?;
        buf.truncate(n.min(buf.len()));
        match self.take(&buf) {
            Ok(new) => {
                for r in new {
                    fstd::warn!("conductor: fault in {} (pid {}): {}", r.service, r.pid, r.message);
                }
                Ok(())
            }
            Err(e) => {
                fstd::error!("conductor: the kernel's fault reports didn't decode: {}", e.describe());
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use fwire::proto::conductor::FaultKind;

    fn r(seq: u64) -> FaultReport {
        FaultReport {
            seq,
            ts_ns: seq * 10,
            pid: 4,
            service: String::from("files"),
            kind: FaultKind::PageFault,
            rip: 0x40_1000,
            addr: 0,
            message: String::from("A page fault (#PF) at 0x401000"),
            frames: alloc::vec![0x40_1000],
            restarted: false,
        }
    }

    fn payload(seqs: core::ops::RangeInclusive<u64>) -> alloc::vec::Vec<u8> {
        fwire::to_payload(&Faults { reports: seqs.map(r).collect(), next_cursor: 0 })
    }

    #[test]
    fn only_new_reports_are_taken_and_only_32_are_kept() {
        let mut k = Kept::new();
        assert_eq!(k.take(&payload(1..=3)).unwrap().len(), 3);
        // The kernel's ring still holds 1..3: none of them twice.
        assert_eq!(k.take(&payload(1..=5)).unwrap().iter().map(|r| r.seq).collect::<alloc::vec::Vec<_>>(), [4, 5]);
        k.take(&payload(6..=40)).unwrap();
        assert_eq!(k.reports.len(), KEEP);
        assert_eq!(k.reports.front().unwrap().seq, 9);
        assert_eq!(k.reports.back().unwrap().seq, 40);
    }

    #[test]
    fn a_damaged_payload_is_refused() {
        let mut k = Kept::new();
        let mut p = payload(1..=2);
        p.truncate(p.len() - 1);
        assert!(k.take(&p).is_err());
        assert!(k.reports.is_empty());
    }
}
