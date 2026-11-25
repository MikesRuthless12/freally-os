// SPDX-License-Identifier: Apache-2.0
//! drv-i8042's logic, host-tested; `main.rs` is the driver.
#![no_std]

extern crate alloc;

pub mod quirks;

/// The status register's bit for a byte waiting in the output buffer.
pub const OUT_FULL: u8 = 1;
/// The status register's bit for a waiting byte that is the mouse's (port 2).
pub const AUX: u8 = 0x20;

/// The controller, as the interrupt loop sees it.
pub trait Controller {
    fn status(&mut self) -> u8;
    fn data(&mut self) -> u8;
    /// Both lines acknowledged: the kernel unmasks IRQs 1 and 12.
    fn ack(&mut self);
}

/// Every byte the controller holds, to `each` (true for the mouse's), read
/// until an ack leaves it empty.
///
/// IRQs 1 and 12 are edge-triggered, and the kernel masks a line from its
/// interrupt until the driver's ack. A byte that lands after the last read
/// but before the ack raises its line while masked, and that edge is lost
/// (an IOAPIC keeps no masked edge, KVM's included). The controller would
/// then hold the byte with its line high and never interrupt again: the
/// keyboard and the mouse both dead until a restart (the soak's round 43).
/// So the status is read again after each ack.
pub fn take_all(c: &mut impl Controller, mut each: impl FnMut(bool, u8)) {
    loop {
        while c.status() & OUT_FULL != 0 {
            let aux = c.status() & AUX != 0;
            each(aux, c.data());
        }
        c.ack();
        if c.status() & OUT_FULL == 0 {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use alloc::vec::Vec;

    /// A controller whose `late` bytes land as the first ack is made: after
    /// the last read, while the kernel still has the line masked.
    struct Fake {
        held: VecDeque<(bool, u8)>,
        late: Vec<(bool, u8)>,
        acks: usize,
    }

    impl Controller for Fake {
        fn status(&mut self) -> u8 {
            self.held.front().map_or(0, |&(aux, _)| OUT_FULL | if aux { AUX } else { 0 })
        }
        fn data(&mut self) -> u8 {
            self.held.pop_front().map_or(0, |(_, b)| b)
        }
        fn ack(&mut self) {
            self.acks += 1;
            self.held.extend(self.late.drain(..));
        }
    }

    #[test]
    fn a_byte_landing_before_the_ack_is_taken_all_the_same() {
        let mut c =
            Fake { held: VecDeque::from([(false, 0x1c), (true, 0x08)]), late: alloc::vec![(false, 0xf0)], acks: 0 };
        let mut got = Vec::new();
        take_all(&mut c, |aux, b| got.push((aux, b)));
        assert_eq!(
            got,
            [(false, 0x1c), (true, 0x08), (false, 0xf0)],
            "the late byte's edge came while masked: no interrupt would ever come for it"
        );
        assert!(c.held.is_empty());
        assert_eq!(c.acks, 2);
    }

    #[test]
    fn an_empty_controller_is_acked_once() {
        let mut c = Fake { held: VecDeque::new(), late: Vec::new(), acks: 0 };
        take_all(&mut c, |_, _| panic!("no byte"));
        assert_eq!(c.acks, 1);
    }
}
