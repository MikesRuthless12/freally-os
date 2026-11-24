// SPDX-License-Identifier: Apache-2.0
//! The fuzz target for a TCP connection (TASK-185): an open connection fed
//! segments with random numbers, flags, windows and data (most near what it
//! expects), mixed with writes, reads, a close and an abort, at times that
//! go forward. It must never panic, and what it sends must stay within the
//! sequence space it has.
//! `ci --slow` runs 2,000 (`FREALLY_FUZZ_ITERS`), each 200 steps.

use fnet::tcp::{Tcp, flag};
use svc_netstack::tcp::{State, Tcb};

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

const MS: u64 = 1_000_000;

#[test]
fn random_segments_never_panic() {
    let iters: u64 = std::env::var("FREALLY_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let mut rng = XorShift(0xD6E8_FEB8_6659_FD93);
    let data = [7u8; 3000];
    for i in 0..iters {
        let iss = rng.next() as u32;
        let peer = rng.next() as u32;
        let run = || {
            let mut rng = XorShift(iss as u64 | 1);
            let mut t = Tcb::connect(iss, 1460, 0);
            t.poll(0);
            let mut sa = Tcp {
                src_port: 7,
                dst_port: 49152,
                seq: peer,
                ack: iss.wrapping_add(1),
                flags: flag::SYN | flag::ACK,
                window: 65535,
                mss: Some(536 + (rng.next() % 1000) as u16),
                wscale: if rng.next() % 2 == 0 { Some((rng.next() % 16) as u8) } else { None },
                data: &[],
            };
            t.segment(&sa, MS);
            let (mut now, mut theirs) = (MS, peer.wrapping_add(1));
            // The end of what the connection last sent (FIN counted): a third
            // of the ACKs say exactly that, as a peer that got everything would.
            let mut sent_end = iss.wrapping_add(1);
            for _ in 0..200 {
                now += rng.next() % (2_000 * MS);
                match rng.next() % 10 {
                    0 => {
                        t.write(&data[..(rng.next() % 3000) as usize]);
                    }
                    1 => {
                        t.read((rng.next() % 4000) as usize);
                    }
                    2 if rng.next() % 8 == 0 => t.close(),
                    3 if rng.next() % 40 == 0 => {
                        t.abort();
                    }
                    _ => {
                        let n = (rng.next() % 1600) as usize;
                        let flags = (rng.next() as u8) & 0x1F | if rng.next() % 4 != 0 { flag::ACK } else { 0 };
                        // Near what it expects most of the time, anywhere sometimes.
                        let seq = if rng.next() % 4 == 0 {
                            rng.next() as u32
                        } else {
                            theirs.wrapping_add((rng.next() % 3000) as u32).wrapping_sub(1000)
                        };
                        let ack = match rng.next() % 3 {
                            0 => sent_end,
                            1 => iss.wrapping_add((rng.next() % 9000) as u32),
                            _ => rng.next() as u32,
                        };
                        sa.seq = seq;
                        sa.ack = ack;
                        sa.flags = flags;
                        sa.window = rng.next() as u16;
                        sa.mss = None;
                        sa.wscale = None;
                        sa.data = &data[..n];
                        t.segment(&sa, now);
                        if seq == theirs && flags & flag::RST == 0 {
                            theirs = theirs.wrapping_add(n as u32);
                        }
                    }
                }
                for o in t.poll(now) {
                    assert!(o.data.len() <= 1460, "a segment past the MSS");
                    let end = o.seq.wrapping_add(o.data.len() as u32 + (o.flags & flag::FIN != 0) as u32);
                    if end.wrapping_sub(sent_end) as i32 > 0 {
                        sent_end = end;
                    }
                }
                if t.state == State::Closed {
                    break;
                }
            }
        };
        if std::panic::catch_unwind(run).is_err() {
            panic!("iteration {i}: the connection panicked (iss {iss:#x}, peer {peer:#x})");
        }
    }
}
