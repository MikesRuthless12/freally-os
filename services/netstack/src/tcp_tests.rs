// SPDX-License-Identifier: Apache-2.0
//! The TCP connection on the host, against segments written here from RFC
//! 9293 and its companions, then over a channel that loses segments both ways.

use alloc::vec::Vec;

use fnet::tcp::{Tcp, flag};

use crate::tcp::{BUFFER, Ended, MSL, Out, RTO_MAX, State, Tcb, isn, lt};

const MS: u64 = 1_000_000;
const S: u64 = 1_000_000_000;
const ISS: u32 = 1000;
const PEER: u32 = 50_000;
const ACK: u8 = flag::ACK;

fn peer(seq: u32, ack: u32, flags: u8, window: u16, data: &[u8]) -> Tcp<'_> {
    Tcp { src_port: 7, dst_port: 49152, seq, ack, flags, window, mss: None, wscale: None, data }
}

/// A connection through its handshake at 10 ms: the peer's MSS 1400, its window `window` scaled by `shift`.
fn open(iss: u32, window: u16, shift: Option<u8>) -> Tcb {
    let mut t = Tcb::connect(iss, 1460, 0);
    let syn = t.poll(0);
    assert_eq!(syn.len(), 1);
    assert_eq!((syn[0].seq, syn[0].flags, syn[0].mss, syn[0].wscale), (iss, flag::SYN, Some(1460), Some(3)));
    let mut sa = peer(PEER, iss.wrapping_add(1), flag::SYN | ACK, window, &[]);
    sa.mss = Some(1400);
    sa.wscale = shift;
    t.segment(&sa, 10 * MS);
    assert_eq!(t.state, State::Established);
    let ack = t.poll(10 * MS);
    assert_eq!(ack.len(), 1);
    assert_eq!((ack[0].seq, ack[0].ack, ack[0].flags), (iss.wrapping_add(1), PEER + 1, ACK));
    t
}

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 + i / 251) as u8).collect()
}

#[test]
fn the_handshake_and_its_options() {
    let t = open(ISS, 65535, Some(7));
    assert_eq!(t.next_wake(), None, "nothing outstanding, no timer");
    // With the peer's scale, our window field is the buffer >> 3; without it, unscaled and capped.
    let mut t = t;
    t.segment(&peer(PEER + 1, ISS + 1, ACK | flag::PSH, 100, b"hi"), 20 * MS);
    let a = t.poll(20 * MS);
    assert_eq!((a[0].ack, a[0].window), (PEER + 3, ((BUFFER - 2) >> 3) as u16));
    let mut u = open(ISS, 65535, None);
    u.segment(&peer(PEER + 1, ISS + 1, ACK, 100, b"hi"), 20 * MS);
    assert_eq!(u.poll(20 * MS)[0].window, 65535);
}

#[test]
fn syns_back_off_then_give_up() {
    let mut t = Tcb::connect(ISS, 1460, 0);
    let mut at = Vec::new();
    let mut now = 0;
    while t.state == State::SynSent {
        for o in t.poll(now) {
            assert_eq!((o.seq, o.flags), (ISS, flag::SYN));
            at.push(now / S);
        }
        match t.next_wake() {
            Some(w) => now = w,
            None => break,
        }
    }
    assert_eq!(at, [0, 1, 3, 7, 15, 31]);
    assert_eq!((t.state, t.ended, now / S), (State::Closed, Some(Ended::TimedOut), 63));
}

#[test]
fn a_refusal_and_stray_answers() {
    let mut t = Tcb::connect(ISS, 1460, 0);
    t.poll(0);
    t.segment(&peer(PEER, ISS, flag::SYN | ACK, 1000, &[]), MS);
    t.segment(&peer(PEER, ISS + 5, flag::SYN | ACK, 1000, &[]), MS);
    t.segment(&peer(0, ISS + 7, flag::RST | ACK, 0, &[]), MS);
    t.segment(&peer(PEER, 0, flag::SYN, 1000, &[]), MS);
    assert_eq!(t.state, State::SynSent, "acks of nothing sent, a RST for another SYN, a bare SYN: all dropped");
    t.segment(&peer(0, ISS + 1, flag::RST | ACK, 0, &[]), MS);
    assert_eq!((t.state, t.ended), (State::Closed, Some(Ended::Refused)));
}

#[test]
fn slow_start_within_both_windows() {
    let mut t = open(ISS, 65535, Some(7));
    let data = payload(100_000);
    assert_eq!(t.write(&data), 100_000);
    let first = t.poll(20 * MS);
    // The initial window for a 1,400-byte MSS is three segments (RFC 5681 § 3.1).
    assert_eq!(
        first.iter().map(|o| (o.seq - ISS - 1, o.data.len())).collect::<Vec<_>>(),
        [(0, 1400), (1400, 1400), (2800, 1400)]
    );
    assert_eq!(first.iter().map(|o| o.data.clone()).collect::<Vec<_>>().concat(), data[..4200]);
    assert_eq!(t.next_wake(), Some(20 * MS + S), "the retransmission timer, at the RTO");
    // One ACK of all three: one more segment's window.
    t.segment(&peer(PEER + 1, ISS + 1 + 4200, ACK, 65535, &[]), 30 * MS);
    let next = t.poll(30 * MS);
    assert_eq!(next.len(), 4);
    assert_eq!(next[0].seq, ISS + 1 + 4200);

    // The peer's window caps the flight: 2,000 bytes unscaled.
    let mut u = open(ISS, 2000, None);
    u.write(&data);
    let sent: usize = u.poll(20 * MS).iter().map(|o| o.data.len()).sum();
    assert_eq!(sent, 2000);
}

#[test]
fn a_timeout_goes_back_to_the_first_unacknowledged_byte() {
    let mut t = open(ISS, 65535, Some(7));
    let data = payload(4200);
    t.write(&data);
    assert_eq!(t.poll(20 * MS).len(), 3);
    // No ACK by the RTO: one segment (the loss window), the RTO doubled.
    let again = t.poll(20 * MS + S);
    assert_eq!(again.iter().map(|o| (o.seq, o.data.len())).collect::<Vec<_>>(), [(ISS + 1, 1400)]);
    assert_eq!(t.next_wake(), Some(20 * MS + 3 * S));
    // The first acknowledged: the next two go (cwnd now two segments).
    t.segment(&peer(PEER + 1, ISS + 1 + 1400, ACK, 65535, &[]), 20 * MS + S + 10 * MS);
    let rest = t.poll(20 * MS + S + 10 * MS);
    assert_eq!(rest.iter().map(|o| (o.seq - ISS - 1, o.data.len())).collect::<Vec<_>>(), [(1400, 1400), (2800, 1400)]);
    assert_eq!(rest.iter().map(|o| o.data.clone()).collect::<Vec<_>>().concat(), data[1400..]);
    // An ACK beyond SND.NXT but within what was sent before the timeout (the
    // first send's segments arrived after all) is taken, not refused.
    t.segment(&peer(PEER + 1, ISS + 1 + 4200, ACK, 65535, &[]), 20 * MS + S + 20 * MS);
    assert_eq!(t.next_wake(), None, "everything acknowledged");
}

#[test]
fn three_duplicates_then_newreno_recovery() {
    let mut t = open(ISS, 65535, Some(7));
    let data = payload(200_000);
    t.write(&data);
    let mut now = 20 * MS;
    // Grow the window: acknowledge every segment until 10 or more are in flight.
    let mut flight: Vec<Out> = t.poll(now);
    while flight.len() < 10 {
        now += 10 * MS;
        let top = flight.last().unwrap();
        t.segment(&peer(PEER + 1, top.seq + top.data.len() as u32, ACK, 65535, &[]), now);
        flight = t.poll(now);
    }
    // Segment 2 of this flight is lost: three duplicate ACKs of its start.
    let hole = flight[2].seq;
    let second = flight[5].seq;
    now += 10 * MS;
    t.segment(&peer(PEER + 1, hole, ACK, 65535, &[]), now);
    for _ in 0..3 {
        t.segment(&peer(PEER + 1, hole, ACK, 65535, &[]), now);
    }
    let fast = t.poll(now);
    assert_eq!(fast[0].seq, hole, "the third duplicate retransmits the hole at once");
    assert_eq!(fast[0].data, flight[2].data);
    // A partial ACK (up to segment 5, also lost): segment 5 goes at once (RFC 6582).
    now += 10 * MS;
    t.segment(&peer(PEER + 1, second, ACK, 65535, &[]), now);
    let partial = t.poll(now);
    assert_eq!(partial[0].seq, second);
    // The full ACK of everything sent before the loss ends recovery.
    let top = flight.last().unwrap();
    let end = top.seq + top.data.len() as u32;
    now += 10 * MS;
    t.segment(&peer(PEER + 1, end, ACK, 65535, &[]), now);
    assert!(!t.poll(now).iter().any(|o| lt(o.seq, end)), "nothing before the recovery point goes again");
}

#[test]
fn receiving_in_order_only() {
    let mut t = open(ISS, 65535, Some(7));
    t.segment(&peer(PEER + 1, ISS + 1, ACK, 512, b"hello "), 20 * MS);
    assert_eq!(t.poll(20 * MS)[0].ack, PEER + 7);
    // Past RCV.NXT: dropped, and the ACK asks again for what's missing.
    t.segment(&peer(PEER + 13, ISS + 1, ACK, 512, b"later"), 21 * MS);
    assert_eq!(t.poll(21 * MS)[0].ack, PEER + 7);
    // Overlapping what came already: only the new part is taken.
    t.segment(&peer(PEER + 4, ISS + 1, ACK, 512, b"lo world"), 22 * MS);
    assert_eq!(t.poll(22 * MS)[0].ack, PEER + 12);
    assert_eq!(t.read(100), b"hello world");
    // Wholly old: acknowledged again, nothing taken.
    t.segment(&peer(PEER + 1, ISS + 1, ACK, 512, b"hello "), 23 * MS);
    assert_eq!(t.poll(23 * MS)[0].ack, PEER + 12);
    assert_eq!(t.readable(), 0);
}

#[test]
fn our_window_closes_and_opens() {
    let mut t = open(ISS, 65535, None);
    let chunk = payload(1400);
    let mut seq = PEER + 1;
    let mut now = 20 * MS;
    while t.readable() < BUFFER {
        let n = chunk.len().min(BUFFER - t.readable());
        t.segment(&peer(seq, ISS + 1, ACK, 512, &chunk[..n]), now);
        seq += n as u32;
        now += MS;
    }
    let full = t.poll(now);
    assert_eq!(full.last().unwrap().window, 0, "the buffer is full: window 0");
    // Data now is past the window: refused, acknowledged where we are.
    t.segment(&peer(seq, ISS + 1, ACK, 512, b"x"), now);
    assert_eq!((t.poll(now)[0].ack, t.readable()), (seq, BUFFER));
    // Reading opens it, and the peer is told at once.
    t.read(BUFFER / 2);
    let update = t.poll(now);
    assert_eq!(update.len(), 1);
    assert_eq!(update[0].window as usize, (BUFFER / 2).min(65535));
}

#[test]
fn the_peers_window_closed_then_probed() {
    let mut t = open(ISS, 0, None);
    t.write(b"waiting");
    assert!(t.poll(20 * MS).is_empty(), "a closed window: nothing sent");
    assert_eq!(t.next_wake(), Some(20 * MS + S), "the persist timer");
    let probe = t.poll(20 * MS + S);
    assert_eq!((probe[0].seq, probe[0].data.as_slice()), (ISS + 1, &b"w"[..]));
    // Still closed (the byte refused, the ACK unchanged): no fast retransmit, probe again later.
    for _ in 0..4 {
        t.segment(&peer(PEER + 1, ISS + 1, ACK, 0, &[]), 20 * MS + S + MS);
    }
    assert!(t.poll(20 * MS + S + MS).is_empty());
    assert_eq!(t.next_wake(), Some(20 * MS + S + S), "the probe's timer");
    let again = t.poll(20 * MS + 2 * S);
    assert_eq!(again[0].data, b"w");
    // It opens: everything goes, from the first byte.
    t.segment(&peer(PEER + 1, ISS + 1, ACK, 1000, &[]), 20 * MS + 2 * S + MS);
    let open = t.poll(20 * MS + 2 * S + MS);
    assert_eq!((open[0].seq, open[0].data.as_slice()), (ISS + 1, &b"waiting"[..]));
}

#[test]
fn closing_first_then_time_wait() {
    let mut t = open(ISS, 65535, Some(7));
    t.write(b"bye");
    t.close();
    let out = t.poll(20 * MS);
    assert_eq!(
        (out[0].data.as_slice(), out[0].flags & flag::FIN),
        (&b"bye"[..], flag::FIN),
        "the FIN rides on the last data"
    );
    assert_eq!(t.state, State::FinWait1);
    assert_eq!(t.write(b"more"), 0, "nothing after close");
    t.segment(&peer(PEER + 1, ISS + 5, ACK, 65535, &[]), 30 * MS);
    assert_eq!(t.state, State::FinWait2);
    t.segment(&peer(PEER + 1, ISS + 5, ACK | flag::FIN, 65535, &[]), 40 * MS);
    assert_eq!(t.state, State::TimeWait);
    assert_eq!(t.poll(40 * MS)[0].ack, PEER + 2, "the peer's FIN acknowledged");
    // Its FIN again (our ACK lost): acknowledged again, TIME-WAIT restarted.
    t.segment(&peer(PEER + 1, ISS + 5, ACK | flag::FIN, 65535, &[]), 50 * MS);
    assert_eq!(t.poll(50 * MS)[0].ack, PEER + 2);
    assert_eq!(t.next_wake(), Some(50 * MS + 2 * MSL));
    t.poll(50 * MS + 2 * MSL);
    assert_eq!((t.state, t.ended), (State::Closed, Some(Ended::Done)));
}

#[test]
fn the_peer_closing_first() {
    let mut t = open(ISS, 65535, Some(7));
    t.segment(&peer(PEER + 1, ISS + 1, ACK | flag::FIN, 65535, b"done"), 20 * MS);
    assert_eq!(t.state, State::CloseWait);
    assert_eq!(t.poll(20 * MS)[0].ack, PEER + 6);
    assert!(!t.at_end());
    assert_eq!(t.read(10), b"done");
    assert!(t.at_end());
    t.write(b"ok");
    t.close();
    let out = t.poll(30 * MS);
    assert_eq!((out[0].data.as_slice(), out[0].flags & flag::FIN), (&b"ok"[..], flag::FIN));
    assert_eq!(t.state, State::LastAck);
    // The FIN lost: it goes again at the RTO, with the data before it.
    let again = t.poll(30 * MS + S);
    assert_eq!((again[0].seq, again[0].flags & flag::FIN, again[0].data.as_slice()), (ISS + 1, flag::FIN, &b"ok"[..]));
    t.segment(&peer(PEER + 6, ISS + 4, ACK, 65535, &[]), 40 * MS + S);
    assert_eq!((t.state, t.ended), (State::Closed, Some(Ended::Done)));
}

#[test]
fn resets_and_syns_are_challenged() {
    let mut t = open(ISS, 65535, Some(7));
    // A RST inside the window but not at RCV.NXT: a challenge ACK (RFC 5961 § 3.2).
    t.segment(&peer(PEER + 100, ISS + 1, flag::RST, 0, &[]), 20 * MS);
    assert_eq!(t.state, State::Established);
    assert_eq!(t.poll(20 * MS)[0].ack, PEER + 1);
    // Outside it: dropped, no answer.
    t.segment(&peer(PEER - 100, ISS + 1, flag::RST, 0, &[]), 21 * MS);
    assert!(t.poll(21 * MS).is_empty());
    // A SYN: a challenge ACK (§ 4.2).
    t.segment(&peer(PEER + 1, 0, flag::SYN, 65535, &[]), 22 * MS);
    assert_eq!((t.state, t.poll(22 * MS)[0].ack), (State::Established, PEER + 1));
    // Exactly at RCV.NXT: reset.
    t.segment(&peer(PEER + 1, 0, flag::RST, 0, &[]), 23 * MS);
    assert_eq!((t.state, t.ended), (State::Closed, Some(Ended::Reset)));
    // Aborting sends a RST at SND.MAX.
    let mut u = open(ISS, 65535, Some(7));
    let rst = u.abort().unwrap();
    assert_eq!((rst.seq, rst.flags & flag::RST), (ISS + 1, flag::RST));
    assert_eq!(u.ended, Some(Ended::Aborted));
}

#[test]
fn an_ack_older_than_any_window_is_challenged() {
    let mut t = open(ISS, 1000, None);
    // Data in the window under an ACK far behind SND.UNA (RFC 5961 § 5.2):
    // not taken, answered with a challenge ACK.
    t.segment(&peer(PEER + 1, ISS.wrapping_sub(100_000), ACK, 1000, b"forged"), 20 * MS);
    assert_eq!(t.readable(), 0);
    assert_eq!(t.poll(20 * MS)[0].ack, PEER + 1);
    // Behind it by less than the window (a late segment): taken.
    t.segment(&peer(PEER + 1, ISS + 1 - 500, ACK, 1000, b"late"), 21 * MS);
    assert_eq!(t.read(10), b"late");
}

#[test]
fn sequence_numbers_wrap() {
    let iss = u32::MAX - 2000;
    let mut t = open(iss, 65535, Some(7));
    let data = payload(4200);
    t.write(&data);
    let out = t.poll(20 * MS);
    assert_eq!(out.len(), 3);
    assert!(out[2].seq < 2000, "past 2^32");
    t.segment(&peer(PEER + 1, out[2].seq + 1400, ACK, 65535, &[]), 30 * MS);
    assert_eq!(t.next_wake(), None, "all acknowledged across the wrap");
}

#[test]
fn initial_sequence_numbers_are_keyed_and_clocked() {
    let k = [7u8; 32];
    let a = isn(&k, &[10, 0, 2, 15], 49152, &[10, 0, 2, 100], 7, 0);
    assert_eq!(
        isn(&k, &[10, 0, 2, 15], 49152, &[10, 0, 2, 100], 7, 4_000 * 1000),
        a.wrapping_add(1000),
        "one every 4 µs"
    );
    assert_ne!(isn(&k, &[10, 0, 2, 15], 49153, &[10, 0, 2, 100], 7, 0), a, "another port");
    assert_ne!(isn(&[8u8; 32], &[10, 0, 2, 15], 49152, &[10, 0, 2, 100], 7, 0), a, "another key");
}

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

/// 300 KB each way over a channel that loses a share of segments in both
/// directions and delays each by 5 ms. The far end is written here: it takes
/// data in order only, answers each segment with an ACK and its own data from
/// the last byte acknowledged (go-back), and on a 200 ms timer of its own.
/// Every byte arrives once, in order, both ways; both sides close.
#[test]
fn bulk_over_a_lossy_channel() {
    for (seed, loss) in [(1u64, 0u64), (2, 2), (3, 10), (4, 25)] {
        let mut rng = XorShift(0x9E37_79B9 ^ seed);
        let mut lost = || rng.next() % 100 < loss;
        let mut t = open(ISS, 65535, Some(7));
        let ours = payload(300_000);
        let theirs: Vec<u8> = ours.iter().map(|b| b ^ 0x5A).collect();
        let (mut written, mut got, mut their_got) = (0usize, Vec::new(), Vec::new());
        let (mut our_fin, mut their_una, mut their_last) = (false, 0usize, 0u64);
        let mut wire: Vec<(u64, Out)> = Vec::new();
        let mut back: Vec<(u64, u32, u32, u8, Vec<u8>)> = Vec::new();
        let mut now = 20 * MS;
        while !matches!(t.state, State::TimeWait | State::Closed) {
            assert!(
                now < 900 * S,
                "seed {seed}, loss {loss}%: stuck in {:?}: {} of ours, {} of theirs",
                t.state,
                their_got.len(),
                got.len()
            );
            written += t.write(&ours[written..]);
            if written == ours.len() {
                t.close();
            }
            for o in t.poll(now) {
                if !lost() {
                    wire.push((now + 5 * MS, o));
                }
            }
            let mut answer = now >= their_last + 200 * MS;
            while let Some(i) = wire.iter().position(|w| w.0 <= now) {
                let (_, o) = wire.remove(i);
                their_una = their_una.max(o.ack.wrapping_sub(PEER + 1) as usize);
                if o.seq == ISS + 1 + their_got.len() as u32 && !our_fin {
                    their_got.extend_from_slice(&o.data);
                    our_fin = o.flags & flag::FIN != 0;
                }
                answer = true;
            }
            if answer && their_una <= theirs.len() {
                let ack = ISS + 1 + their_got.len() as u32 + our_fin as u32;
                let data = &theirs[their_una..(their_una + 1400).min(theirs.len())];
                let fin = our_fin && their_una + data.len() == theirs.len();
                if !lost() {
                    back.push((
                        now + 5 * MS,
                        PEER + 1 + their_una as u32,
                        ack,
                        ACK | if fin { flag::FIN } else { 0 },
                        data.to_vec(),
                    ));
                }
                their_last = now;
            }
            while let Some(i) = back.iter().position(|b| b.0 <= now) {
                let (_, seq, ack, flags, data) = back.remove(i);
                t.segment(&peer(seq, ack, flags, 65535, &data), now);
                got.extend(t.read(usize::MAX));
            }
            now = match t.next_wake() {
                Some(w) if w > now => w.min(now + MS),
                _ => now + MS,
            };
        }
        assert_eq!(t.ended, None, "seed {seed}, loss {loss}%: closed in TIME-WAIT, not given up");
        assert!(their_got == ours, "seed {seed}, loss {loss}%: ours arrived whole and in order");
        assert!(got == theirs, "seed {seed}, loss {loss}%: theirs arrived whole and in order");
        assert!(t.rto() <= RTO_MAX);
    }
}

/// A peer's SYN from `PEER`: its MSS 1400, its window 29200, its scale `shift`.
fn syn(shift: Option<u8>) -> Tcp<'static> {
    let mut s = peer(PEER, 0, flag::SYN, 29200, &[]);
    s.mss = Some(1400);
    s.wscale = shift;
    s
}

/// The passive open (RFC 9293 § 3.10.7.2, TASK-184): a listener's SYN-ACK,
/// then the peer's ACK makes it ESTABLISHED, its data taken with it.
#[test]
fn a_listener_takes_a_handshake() {
    let mut t = Tcb::accept(ISS, 1460, &syn(Some(7)), 0);
    assert_eq!(t.state, State::SynReceived);
    let sa = t.poll(0);
    assert_eq!(sa.len(), 1);
    assert_eq!(
        (sa[0].seq, sa[0].ack, sa[0].flags, sa[0].mss, sa[0].wscale),
        (ISS, PEER + 1, flag::SYN | ACK, Some(1460), Some(3))
    );
    assert_eq!(sa[0].window, 65535, "a SYN's window is never scaled");
    // The handshake's ACK, carrying data and the peer's scaled window.
    t.segment(&peer(PEER + 1, ISS + 1, ACK | flag::PSH, 100, b"hi"), 10 * MS);
    assert_eq!(t.state, State::Established);
    assert_eq!(t.read(10), b"hi");
    let a = t.poll(10 * MS);
    assert_eq!((a[0].seq, a[0].ack, a[0].flags), (ISS + 1, PEER + 3, ACK));
    assert_eq!(a[0].window, ((BUFFER) >> 3) as u16, "our window scaled, the peer having offered scaling");
    // What it writes goes out, within the peer's window (100 << 7).
    assert_eq!(t.write(&payload(5000)), 5000);
    let sent: usize = t.poll(20 * MS).iter().map(|o| o.data.len()).sum();
    assert!(sent > 0 && sent <= 12_800, "{sent} bytes in the first flight");
}

/// A peer that offers no window scale gets none back, and an unscaled window.
#[test]
fn no_scale_offered_none_given() {
    let mut t = Tcb::accept(ISS, 1460, &syn(None), 0);
    assert_eq!(t.poll(0)[0].wscale, None);
    t.segment(&peer(PEER + 1, ISS + 1, ACK, 100, b"x"), 10 * MS);
    assert_eq!(t.poll(10 * MS)[0].window, 65535);
}

/// No ACK: the SYN-ACK again at 1, 3, 7, 15 and 31 s, given up at 63 s.
#[test]
fn syn_acks_back_off_then_give_up() {
    let mut t = Tcb::accept(ISS, 1460, &syn(Some(7)), 0);
    let mut at = Vec::new();
    let mut now = 0;
    while t.state != State::Closed && now < 100 * S {
        if t.poll(now).iter().any(|o| o.flags == flag::SYN | ACK) {
            at.push(now / S);
        }
        now = t.next_wake().unwrap_or(now + S).max(now + MS);
    }
    assert_eq!(at, [0, 1, 3, 7, 15, 31]);
    assert_eq!((t.state, t.ended), (State::Closed, Some(Ended::TimedOut)));
}

/// The peer's SYN again (our SYN-ACK was lost) brings the SYN-ACK again at once.
#[test]
fn the_same_syn_again_brings_the_syn_ack_again() {
    let mut t = Tcb::accept(ISS, 1460, &syn(Some(7)), 0);
    t.poll(0);
    t.segment(&syn(Some(7)), 200 * MS);
    let again = t.poll(200 * MS);
    assert_eq!(again.iter().map(|o| (o.seq, o.flags)).collect::<Vec<_>>(), [(ISS, flag::SYN | ACK)]);
}

/// An ACK of something never sent is answered with a RST at its number
/// (§ 3.10.7.4), and the connection waits on; a RST at RCV.NXT ends it.
#[test]
fn a_wrong_ack_is_reset_and_a_reset_ends_it() {
    let mut t = Tcb::accept(ISS, 1460, &syn(Some(7)), 0);
    t.poll(0);
    t.segment(&peer(PEER + 1, ISS + 9, ACK, 100, &[]), 10 * MS);
    assert_eq!(t.state, State::SynReceived);
    let r = t.poll(10 * MS);
    assert_eq!(r.iter().map(|o| (o.seq, o.flags)).collect::<Vec<_>>(), [(ISS + 9, flag::RST)]);
    t.segment(&peer(PEER + 1, 0, flag::RST, 0, &[]), 20 * MS);
    assert_eq!((t.state, t.ended), (State::Closed, Some(Ended::Reset)));
}

/// Data written between polls is due at once (TASK-184: netstack takes a
/// SEND's bytes after polling, and a client with its RECV already waiting
/// asks nothing more, so nothing else wakes it); so is a FIN. A window that
/// can't take it waits for its timers, never a spin.
#[test]
fn data_written_is_due_at_once() {
    let mut t = open(ISS, 65535, Some(7));
    assert_eq!(t.next_wake(), None);
    t.write(b"hello");
    assert_eq!(t.next_wake(), Some(0), "written, not yet sent");
    assert_eq!(t.poll(30 * MS).iter().map(|o| o.data.len()).sum::<usize>(), 5);
    assert_ne!(t.next_wake(), Some(0), "sent: only the retransmission timer");
    t.close();
    assert_eq!(t.next_wake(), Some(0), "the FIN, queued");
    t.poll(40 * MS);
    assert_ne!(t.next_wake(), Some(0));
    // The peer's window closed: written data waits for the persist timer.
    let mut z = open(ISS, 0, None);
    z.write(b"held");
    assert_ne!(z.next_wake(), Some(0), "a closed window: no wake at once");
}
