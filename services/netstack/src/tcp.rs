// SPDX-License-Identifier: Apache-2.0
//! TCP connections (TASK-177), outgoing only, tested on the host: RFC 9293's
//! states and its segment-arrives rules, the initial sequence number from a
//! keyed hash (RFC 6528), retransmission (RFC 6298, Karn's rule), the window
//! and its scale (RFC 7323), slow start, congestion avoidance and fast
//! retransmit (RFC 5681) with NewReno's recovery (RFC 6582), RST and SYN
//! answered with challenge ACKs (RFC 5961), the zero-window probe, TIME-WAIT;
//! and the passive open from a listener's SYN ([`Tcb::accept`], TASK-184).
//! No keep-alive. A [`Tcb`] knows sequence numbers, not
//! addresses: netstack wraps what it sends in IP. Times are nanoseconds on one
//! monotonic clock.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use fnet::tcp::{Tcp, flag};

const SECOND: u64 = 1_000_000_000;
/// Maximum segment lifetime (RFC 9293 § 3.4.2); TIME-WAIT is twice it.
pub const MSL: u64 = 30 * SECOND;
pub const RTO_INITIAL: u64 = SECOND;
pub const RTO_MIN: u64 = SECOND;
pub const RTO_MAX: u64 = 60 * SECOND;
/// SYNs sent before giving up: at 0, 1, 3, 7, 15 and 31 s; given up at 63 s.
pub const SYN_TRIES: u32 = 6;
/// Retransmissions of data before the connection is given up.
pub const DATA_TRIES: u32 = 12;
/// Each direction's buffer.
pub const BUFFER: usize = 256 * 1024;
/// The window scale offered: 256 KiB >> 3 fits in 16 bits.
pub const WSCALE: u8 = 3;
/// The MSS assumed when the peer names none (RFC 9293 § 3.7.1).
pub const DEFAULT_MSS: u16 = 536;

/// Sequence-number order modulo 2^32 (RFC 9293 § 3.4).
pub fn lt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

pub fn le(a: u32, b: u32) -> bool {
    !lt(b, a)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    SynSent,
    /// A listener's: its SYN-ACK sent, the handshake's ACK awaited.
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    Closing,
    TimeWait,
    CloseWait,
    LastAck,
    Closed,
}

/// Why a connection closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// Both sides closed.
    Done,
    /// The peer refused the connection (a RST to the SYN).
    Refused,
    /// The peer reset it.
    Reset,
    /// No answer: the SYN or data retransmitted until given up.
    TimedOut,
    /// This side aborted it.
    Aborted,
}

/// A segment to send: sequence space only (netstack adds ports and addresses).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Out {
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub window: u16,
    pub mss: Option<u16>,
    pub wscale: Option<u8>,
    pub data: Vec<u8>,
}

/// RFC 6528's initial sequence number: a 4 µs clock plus a keyed hash of the
/// connection's addresses and ports, so one connection's numbers say nothing
/// of another's.
pub fn isn(secret: &[u8; 32], local: &[u8], local_port: u16, remote: &[u8], remote_port: u16, now: u64) -> u32 {
    let mut h = fcrypto::hkdf::Hmac::new(secret);
    h.update(local);
    h.update(&local_port.to_be_bytes());
    h.update(remote);
    h.update(&remote_port.to_be_bytes());
    let f = h.finish();
    ((now / 4_000) as u32).wrapping_add(u32::from_be_bytes([f[0], f[1], f[2], f[3]]))
}

/// One connection's transmission control block (RFC 9293 § 3.3.1).
pub struct Tcb {
    pub state: State,
    pub ended: Option<Ended>,
    iss: u32,
    snd_una: u32,
    snd_nxt: u32,
    /// The highest sequence number sent (SND.NXT goes back to SND.UNA after a timeout).
    snd_max: u32,
    /// The peer's window, in bytes (its field shifted by its scale).
    snd_wnd: u32,
    /// The largest window the peer has offered (RFC 5961 § 5.2's MAX.SND.WND).
    max_snd_wnd: u32,
    snd_wl1: u32,
    snd_wl2: u32,
    snd_shift: u8,
    rcv_shift: u8,
    /// Bytes from SND.UNA on: sent and unacknowledged, then not yet sent.
    send: VecDeque<u8>,
    /// close() was called: a FIN follows the data.
    fin_queued: bool,
    /// The FIN is at or before SND.NXT.
    fin_sent: bool,
    mss: u16,
    our_mss: u16,
    cwnd: u32,
    ssthresh: u32,
    dupacks: u32,
    /// NewReno's recovery point; Some while in fast recovery.
    recover: Option<u32>,
    retransmit: bool,
    srtt: Option<u64>,
    rttvar: u64,
    /// The RTO from the measurements (RFC 6298 § 2).
    rto: u64,
    /// Timeouts since the last ACK of new data: the RTO is doubled this many
    /// times (§ 5.5), and an ACK of new data clears it, since it shows the path works.
    backoff: u32,
    /// The segment being timed: its first sequence number and when it went.
    timing: Option<(u32, u64)>,
    timer: Option<u64>,
    tries: u32,
    rcv_nxt: u32,
    recv: VecDeque<u8>,
    fin_received: bool,
    ack_now: bool,
    /// A byte past a closed window is out, to learn when it opens (RFC 9293 § 3.8.6.1).
    probe: bool,
    time_wait_until: Option<u64>,
    /// In SYN-RECEIVED, an ACK of what was never sent: a RST at its number goes out.
    reset_at: Option<u32>,
}

impl Tcb {
    /// Opens a connection: the SYN goes out at the next [`Tcb::poll`].
    pub fn connect(iss: u32, our_mss: u16, now: u64) -> Tcb {
        Tcb {
            state: State::SynSent,
            ended: None,
            iss,
            snd_una: iss,
            snd_nxt: iss,
            snd_max: iss,
            snd_wnd: 0,
            max_snd_wnd: 0,
            snd_wl1: 0,
            snd_wl2: 0,
            snd_shift: 0,
            rcv_shift: 0,
            send: VecDeque::new(),
            fin_queued: false,
            fin_sent: false,
            mss: DEFAULT_MSS,
            our_mss,
            cwnd: 0,
            ssthresh: u32::MAX,
            dupacks: 0,
            recover: None,
            retransmit: false,
            srtt: None,
            rttvar: 0,
            rto: RTO_INITIAL,
            backoff: 0,
            timing: None,
            timer: Some(now),
            tries: 0,
            rcv_nxt: 0,
            recv: VecDeque::new(),
            fin_received: false,
            ack_now: false,
            probe: false,
            time_wait_until: None,
            reset_at: None,
        }
    }

    /// A connection from a peer's SYN to a listener (RFC 9293 § 3.10.7.2):
    /// SYN-RECEIVED, its SYN-ACK out at the next [`Tcb::poll`] and again on
    /// the SYN's timers; the window scale offered only if the SYN offered one
    /// (RFC 7323 § 1.3). Data on the SYN isn't taken: the peer sends it again.
    pub fn accept(iss: u32, our_mss: u16, syn: &Tcp, now: u64) -> Tcb {
        let mut t = Tcb::connect(iss, our_mss, now);
        t.state = State::SynReceived;
        t.rcv_nxt = syn.seq.wrapping_add(1);
        if let Some(s) = syn.wscale {
            t.snd_shift = s;
            t.rcv_shift = WSCALE;
        }
        t.snd_wnd = syn.window as u32;
        t.max_snd_wnd = t.snd_wnd;
        t.snd_wl1 = syn.seq;
        t.mss = syn.mss.unwrap_or(DEFAULT_MSS).clamp(64, our_mss);
        t
    }

    /// Bytes taken into the send buffer (fewer than given once it's full).
    pub fn write(&mut self, data: &[u8]) -> usize {
        if !matches!(self.state, State::SynSent | State::SynReceived | State::Established | State::CloseWait)
            || self.fin_queued
        {
            return 0;
        }
        let n = data.len().min(BUFFER - self.send.len());
        self.send.extend(&data[..n]);
        n
    }

    /// Up to `max` bytes received, in order.
    pub fn read(&mut self, max: usize) -> Vec<u8> {
        let before = self.window_bytes();
        let n = max.min(self.recv.len());
        let out: Vec<u8> = self.recv.drain(..n).collect();
        // The peer hears once the window has opened by a segment, or by half
        // the buffer (RFC 9293 § 3.8.6.2.2).
        let after = self.window_bytes();
        let mss = self.mss as usize;
        if n > 0 && ((before < mss && after >= mss) || after - before >= BUFFER / 2) {
            self.ack_now = true;
        }
        out
    }

    /// The peer has closed its side and every byte it sent has been read.
    pub fn at_end(&self) -> bool {
        self.fin_received && self.recv.is_empty()
    }

    pub fn readable(&self) -> usize {
        self.recv.len()
    }

    /// Room left in the send buffer.
    pub fn writable(&self) -> usize {
        if matches!(self.state, State::SynSent | State::Established | State::CloseWait) && !self.fin_queued {
            BUFFER - self.send.len()
        } else {
            0
        }
    }

    /// Closes this side: a FIN follows whatever is still to send.
    pub fn close(&mut self) {
        match self.state {
            State::SynSent => self.end(Ended::Done),
            State::Established | State::CloseWait => self.fin_queued = true,
            _ => {}
        }
    }

    /// Ends the connection at once: the RST to send, if the peer knows of it.
    pub fn abort(&mut self) -> Option<Out> {
        let synced = !matches!(self.state, State::SynSent | State::TimeWait | State::Closed);
        let rst = synced.then(|| Out { flags: flag::RST, ..self.out(self.snd_max, 0, Vec::new()) });
        self.end(Ended::Aborted);
        rst
    }

    fn end(&mut self, why: Ended) {
        self.state = State::Closed;
        self.ended.get_or_insert(why);
        self.timer = None;
        self.time_wait_until = None;
        self.send.clear();
    }

    fn window_bytes(&self) -> usize {
        BUFFER - self.recv.len()
    }

    fn out(&self, seq: u32, flags: u8, data: Vec<u8>) -> Out {
        let syn = flags & flag::SYN != 0;
        // A SYN's window is never scaled (RFC 7323 § 2.2).
        let shift = if syn { 0 } else { self.rcv_shift };
        let synced = self.state != State::SynSent;
        Out {
            seq,
            ack: if synced { self.rcv_nxt } else { 0 },
            flags: if synced { flags | flag::ACK } else { flags },
            window: (self.window_bytes() >> shift).min(65535) as u16,
            mss: syn.then_some(self.our_mss),
            // Offered on our SYN; on a SYN-ACK only if the peer's SYN offered it.
            wscale: (syn && (self.state == State::SynSent || self.rcv_shift != 0)).then_some(WSCALE),
            data,
        }
    }

    /// The retransmission timeout now: the measured RTO, backed off.
    pub fn rto(&self) -> u64 {
        self.rto.saturating_mul(1 << self.backoff.min(16)).min(RTO_MAX)
    }

    /// When [`Tcb::poll`] next has something to do: at once while data or a
    /// FIN waits that the windows let go (written since the last poll: TASK-184
    /// found netstack asleep with a SEND's bytes taken and not sent), else its timers.
    pub fn next_wake(&self) -> Option<u64> {
        if self.can_send() {
            return Some(0);
        }
        match (self.timer, self.time_wait_until) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Whether [`Tcb::send_new`] would send now: its own conditions, so a wake
    /// for it is never a spin (a closed window waits for its timers).
    fn can_send(&self) -> bool {
        if !matches!(
            self.state,
            State::Established | State::CloseWait | State::FinWait1 | State::Closing | State::LastAck
        ) || self.fin_sent
            || self.probe
        {
            return false;
        }
        let offset = self.flight() as usize;
        let unsent = self.send.len().saturating_sub(offset);
        let window = self.cwnd.min(self.snd_wnd) as usize;
        (unsent > 0 && offset < window) || (self.fin_queued && offset == self.send.len())
    }

    fn flight(&self) -> u32 {
        self.snd_nxt.wrapping_sub(self.snd_una)
    }

    /// The FIN's sequence number, once queued: after every byte of data.
    fn fin_seq(&self) -> u32 {
        self.snd_una.wrapping_add(self.send.len() as u32)
    }

    /// The initial window (RFC 5681 § 3.1).
    fn initial_window(&self) -> u32 {
        let m = self.mss as u32;
        match m {
            2191.. => 2 * m,
            1096..=2190 => 3 * m,
            _ => 4 * m,
        }
    }

    fn restart_timer(&mut self, now: u64) {
        self.timer = (self.flight() > 0).then_some(now + self.rto());
    }

    /// RFC 6298 § 2.2–2.3: a round trip measured updates SRTT, RTTVAR and the RTO.
    fn measured(&mut self, r: u64) {
        let s = match self.srtt {
            None => {
                self.rttvar = r / 2;
                r
            }
            Some(s) => {
                self.rttvar = (3 * self.rttvar + s.abs_diff(r)) / 4;
                (7 * s + r) / 8
            }
        };
        self.srtt = Some(s);
        self.rto = (s + (4 * self.rttvar).max(1_000_000)).clamp(RTO_MIN, RTO_MAX);
    }

    /// A segment from the peer (RFC 9293 § 3.10.7.4; in SYN-SENT, § 3.10.7.3).
    pub fn segment(&mut self, seg: &Tcp, now: u64) {
        match self.state {
            State::Closed => return,
            State::SynSent => return self.syn_sent(seg, now),
            _ => {}
        }
        if self.state == State::SynReceived && seg.flags & flag::SYN != 0 && seg.seq.wrapping_add(1) == self.rcv_nxt {
            // The peer's SYN again: our SYN-ACK was lost. It goes again now
            // (a try of the SYN's, so a peer sending SYNs can't keep it here).
            self.timer = Some(now);
            return;
        }
        let len = seg.seq_len();
        let wnd = self.window_bytes() as u32;
        let rcv_nxt = self.rcv_nxt;
        let in_window = |s: u32| le(rcv_nxt, s) && lt(s, rcv_nxt.wrapping_add(wnd));
        let acceptable = match (len, wnd) {
            (0, 0) => seg.seq == rcv_nxt,
            (0, _) => in_window(seg.seq),
            (_, 0) => false,
            _ => in_window(seg.seq) || in_window(seg.seq.wrapping_add(len - 1)),
        };
        if seg.flags & flag::RST != 0 {
            // RFC 5961 § 3.2: a RST at exactly RCV.NXT resets; one elsewhere in
            // the window gets a challenge ACK; any other is dropped.
            if seg.seq == rcv_nxt {
                self.end(Ended::Reset);
            } else if acceptable {
                self.ack_now = true;
            }
            return;
        }
        if !acceptable {
            // Old, or past the window (a zero-window probe among them): our ACK says where we are.
            self.ack_now = true;
            if self.state == State::TimeWait && seg.flags & flag::FIN != 0 {
                self.time_wait(now); // its FIN again: our last ACK was lost
            }
            return;
        }
        if seg.flags & flag::SYN != 0 {
            self.ack_now = true; // RFC 5961 § 4.2: a SYN in a synchronized state gets a challenge ACK
            return;
        }
        if self.state == State::SynReceived && seg.flags & flag::ACK != 0 && !self.syn_acked(seg, now) {
            return;
        }
        if seg.flags & flag::ACK == 0 || !self.ack(seg, now) {
            return;
        }
        // Data and the FIN, in order only: anything past RCV.NXT is dropped
        // and asked for again by the ACK.
        if lt(self.rcv_nxt, seg.seq) {
            self.ack_now = true;
            return;
        }
        let skip = self.rcv_nxt.wrapping_sub(seg.seq) as usize;
        let receiving = matches!(self.state, State::Established | State::FinWait1 | State::FinWait2);
        if !seg.data.is_empty() {
            self.ack_now = true;
        }
        if receiving && seg.data.len() > skip {
            let new = &seg.data[skip..];
            let n = new.len().min(self.window_bytes());
            self.recv.extend(&new[..n]);
            self.rcv_nxt = self.rcv_nxt.wrapping_add(n as u32);
            if n < new.len() {
                return; // the rest, and a FIN after it, come again
            }
        }
        let fin_at = seg.seq.wrapping_add(seg.data.len() as u32);
        if seg.flags & flag::FIN != 0 && fin_at == self.rcv_nxt && !self.fin_received && receiving {
            self.fin_received = true;
            self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
            self.ack_now = true;
            match self.state {
                State::Established => self.state = State::CloseWait,
                State::FinWait1 => self.state = State::Closing,
                _ => self.time_wait(now),
            }
        }
    }

    /// SYN-RECEIVED's ACK (§ 3.10.7.4): one of our SYN makes the connection
    /// ESTABLISHED (false for any other: a RST at its number goes out).
    fn syn_acked(&mut self, seg: &Tcp, now: u64) -> bool {
        if !(lt(self.snd_una, seg.ack) && le(seg.ack, self.snd_max)) {
            self.reset_at = Some(seg.ack);
            return false;
        }
        // The SYN's number is taken: it was never in the send buffer.
        self.snd_una = self.iss.wrapping_add(1);
        if let Some((_, sent)) = self.timing.take() {
            self.measured(now - sent);
        }
        self.cwnd = self.initial_window();
        self.tries = 0;
        self.backoff = 0;
        self.state = State::Established;
        self.restart_timer(now);
        true
    }

    fn time_wait(&mut self, now: u64) {
        self.state = State::TimeWait;
        self.timer = None;
        self.time_wait_until = Some(now + 2 * MSL);
    }

    fn syn_sent(&mut self, seg: &Tcp, now: u64) {
        let has_ack = seg.flags & flag::ACK != 0;
        if has_ack && (le(seg.ack, self.iss) || lt(self.snd_max, seg.ack)) {
            return; // not about this SYN
        }
        if seg.flags & flag::RST != 0 {
            if has_ack {
                self.end(Ended::Refused);
            }
            return;
        }
        // A SYN without an ACK (a simultaneous open) never answers an
        // outgoing-only client's SYN: dropped.
        if seg.flags & flag::SYN == 0 || !has_ack {
            return;
        }
        self.rcv_nxt = seg.seq.wrapping_add(1);
        self.snd_una = seg.ack;
        self.snd_nxt = seg.ack;
        if let Some(s) = seg.wscale {
            self.snd_shift = s;
            self.rcv_shift = WSCALE;
        }
        self.snd_wnd = seg.window as u32;
        self.max_snd_wnd = self.snd_wnd;
        self.snd_wl1 = seg.seq;
        self.snd_wl2 = seg.ack;
        self.mss = seg.mss.unwrap_or(DEFAULT_MSS).clamp(64, self.our_mss);
        self.cwnd = self.initial_window();
        if let Some((_, sent)) = self.timing.take() {
            self.measured(now - sent);
        }
        self.tries = 0;
        self.state = State::Established;
        self.ack_now = true;
        self.restart_timer(now);
    }

    /// The ACK field (§ 3.10.7.4, fifth check): false when the segment goes no further.
    fn ack(&mut self, seg: &Tcp, now: u64) -> bool {
        // Past what was sent, or older than SND.UNA by more than any window
        // the peer offered (RFC 5961 § 5.2): no peer's ACK. A challenge ACK
        // answers it, so a blind injection must hit the ACK's range too.
        if lt(self.snd_max, seg.ack) || lt(seg.ack, self.snd_una.wrapping_sub(self.max_snd_wnd)) {
            self.ack_now = true;
            return false;
        }
        let window = (seg.window as u32) << self.snd_shift;
        let fin_seq = self.fin_seq();
        if lt(self.snd_una, seg.ack) {
            let acked = seg.ack.wrapping_sub(self.snd_una);
            let data = (acked as usize).min(self.send.len());
            self.send.drain(..data);
            self.snd_una = seg.ack;
            if lt(self.snd_nxt, seg.ack) {
                self.snd_nxt = seg.ack; // acknowledged before a timeout's resend reached it
            }
            if self.fin_queued && seg.ack == fin_seq.wrapping_add(1) {
                self.fin_sent = true;
            }
            if let Some((s, sent)) = self.timing
                && lt(s, seg.ack)
            {
                self.measured(now - sent);
                self.timing = None;
            }
            self.tries = 0;
            self.backoff = 0;
            self.probe = false;
            self.congestion_on_ack(acked);
            self.restart_timer(now);
        } else if seg.ack == self.snd_una
            && seg.data.is_empty()
            && seg.flags & flag::FIN == 0
            && window == self.snd_wnd
            && self.flight() > 0
            && !self.probe
        {
            self.duplicate_ack();
        }
        // The send window, from the newest segment (SND.WL1, SND.WL2).
        if lt(self.snd_wl1, seg.seq) || (self.snd_wl1 == seg.seq && le(self.snd_wl2, seg.ack)) {
            self.snd_wnd = window;
            self.max_snd_wnd = self.max_snd_wnd.max(window);
            self.snd_wl1 = seg.seq;
            self.snd_wl2 = seg.ack;
            if self.probe && window > 0 {
                // The window opened with the probe's byte refused: it goes again as data.
                self.probe = false;
                self.snd_nxt = self.snd_una;
                self.timer = None;
            }
        }
        let fin_acked = self.fin_queued && seg.ack == fin_seq.wrapping_add(1);
        match self.state {
            State::FinWait1 if fin_acked => self.state = State::FinWait2,
            State::Closing if fin_acked => self.time_wait(now),
            State::LastAck if fin_acked => {
                self.end(Ended::Done);
                return false;
            }
            _ => {}
        }
        true
    }

    /// RFC 5681 § 3.1 and RFC 6582 § 3.2, on an ACK of `acked` new bytes.
    fn congestion_on_ack(&mut self, acked: u32) {
        let mss = self.mss as u32;
        self.dupacks = 0;
        match self.recover {
            Some(r) if lt(self.snd_una, r) => {
                // A partial ACK: the next hole goes again at once, the window deflated.
                self.cwnd = self.cwnd.saturating_sub(acked).saturating_add(mss);
                self.retransmit = true;
            }
            Some(_) => {
                // Everything sent before the loss is acknowledged: recovery ends.
                self.cwnd = self.ssthresh.min(self.flight().max(mss) + mss);
                self.recover = None;
            }
            None if self.cwnd < self.ssthresh => self.cwnd = self.cwnd.saturating_add(acked.min(mss)),
            None => self.cwnd = self.cwnd.saturating_add((mss * mss / self.cwnd.max(1)).max(1)),
        }
    }

    /// RFC 5681 § 3.2: the third duplicate ACK retransmits; more inflate the window.
    fn duplicate_ack(&mut self) {
        let mss = self.mss as u32;
        self.dupacks += 1;
        if self.recover.is_some() {
            self.cwnd = self.cwnd.saturating_add(mss);
        } else if self.dupacks == 3 {
            self.ssthresh = (self.flight() / 2).max(2 * mss);
            self.recover = Some(self.snd_max);
            self.cwnd = self.ssthresh + 3 * mss;
            self.retransmit = true;
            self.timing = None; // Karn: no sample across a retransmission
        }
    }

    /// What's due at `now`: the timers, a retransmission, new data, a FIN, an ACK.
    pub fn poll(&mut self, now: u64) -> Vec<Out> {
        let mut out = Vec::new();
        if self.time_wait_until.is_some_and(|t| now >= t) {
            self.end(Ended::Done);
        }
        if self.state == State::Closed {
            return out;
        }
        if self.timer.is_some_and(|t| now >= t) {
            self.timeout(now, &mut out);
            if self.state == State::Closed {
                return out;
            }
        }
        if let Some(seq) = self.reset_at.take() {
            out.push(Out { seq, ack: 0, flags: flag::RST, window: 0, mss: None, wscale: None, data: Vec::new() });
        }
        if self.retransmit {
            self.retransmit = false;
            self.retransmit_first(&mut out);
        }
        if matches!(
            self.state,
            State::Established | State::CloseWait | State::FinWait1 | State::Closing | State::LastAck
        ) {
            self.send_new(now, &mut out);
        }
        if self.ack_now && out.is_empty() && self.state != State::SynSent {
            out.push(self.out(self.snd_nxt, 0, Vec::new()));
        }
        self.ack_now = false;
        out
    }

    fn timeout(&mut self, now: u64, out: &mut Vec<Out>) {
        if matches!(self.state, State::SynSent | State::SynReceived) {
            if self.tries >= SYN_TRIES {
                return self.end(Ended::TimedOut);
            }
            if self.tries == 0 {
                self.timing = Some((self.iss, now));
            } else {
                self.timing = None;
                self.backoff += 1;
            }
            self.tries += 1;
            self.snd_nxt = self.iss.wrapping_add(1);
            self.snd_max = self.snd_nxt;
            out.push(self.out(self.iss, flag::SYN, Vec::new()));
            self.timer = Some(now + self.rto());
            return;
        }
        if self.flight() == 0 && !self.probe {
            // The persist timer: a byte past the closed window.
            if self.snd_wnd == 0 && !self.send.is_empty() {
                let byte = self.send[0];
                out.push(self.out(self.snd_nxt, flag::PSH, alloc::vec![byte]));
                self.snd_nxt = self.snd_nxt.wrapping_add(1);
                if lt(self.snd_max, self.snd_nxt) {
                    self.snd_max = self.snd_nxt;
                }
                self.probe = true;
                self.timer = Some(now + self.rto());
            } else {
                self.timer = None;
            }
            return;
        }
        if self.probe {
            // The window is still closed: probe again, backing off, for as long as the peer answers.
            self.backoff += 1;
            self.retransmit_first(out);
            self.timer = Some(now + self.rto());
            return;
        }
        if self.tries >= DATA_TRIES {
            return self.end(Ended::TimedOut);
        }
        self.tries += 1;
        // RFC 5681 § 3.1, (4): one segment's window; everything after SND.UNA goes again.
        self.ssthresh = (self.flight() / 2).max(2 * self.mss as u32);
        self.cwnd = self.mss as u32;
        self.recover = None;
        self.dupacks = 0;
        self.timing = None;
        self.backoff += 1;
        self.snd_nxt = self.snd_una;
        self.fin_sent = false;
        self.timer = None;
    }

    /// The first unacknowledged segment again: data from SND.UNA, or the FIN alone.
    fn retransmit_first(&mut self, out: &mut Vec<Out>) {
        let n = (self.flight() as usize).min(self.send.len()).min(self.mss as usize);
        let data: Vec<u8> = self.send.iter().take(n).copied().collect();
        let fin = self.fin_sent && self.snd_una.wrapping_add(n as u32) == self.fin_seq();
        if n > 0 || fin {
            out.push(self.out(self.snd_una, if fin { flag::FIN } else { flag::PSH }, data));
        }
    }

    /// New data as the windows allow, then the FIN once every byte has gone.
    fn send_new(&mut self, now: u64, out: &mut Vec<Out>) {
        if self.fin_sent || self.probe {
            return;
        }
        let mss = self.mss as usize;
        loop {
            let offset = self.flight() as usize;
            let unsent = self.send.len().saturating_sub(offset);
            let window = self.cwnd.min(self.snd_wnd) as usize;
            if unsent == 0 || offset >= window {
                break;
            }
            let n = unsent.min(mss).min(window - offset);
            let data: Vec<u8> = self.send.range(offset..offset + n).copied().collect();
            let last = n == unsent;
            let fin = last && self.fin_queued;
            let flags = if last { flag::PSH } else { 0 } | if fin { flag::FIN } else { 0 };
            out.push(self.out(self.snd_nxt, flags, data));
            if self.timing.is_none() && self.snd_nxt == self.snd_max {
                self.timing = Some((self.snd_nxt, now));
            }
            self.advance(n as u32 + fin as u32, now);
            if fin {
                return self.fin_out();
            }
        }
        if self.fin_queued && self.flight() as usize == self.send.len() {
            out.push(self.out(self.snd_nxt, flag::FIN, Vec::new()));
            self.advance(1, now);
            self.fin_out();
        }
        if self.snd_wnd == 0 && self.flight() == 0 && !self.send.is_empty() && self.timer.is_none() {
            self.timer = Some(now + self.rto()); // the persist timer
        }
    }

    fn advance(&mut self, n: u32, now: u64) {
        self.snd_nxt = self.snd_nxt.wrapping_add(n);
        if lt(self.snd_max, self.snd_nxt) {
            self.snd_max = self.snd_nxt;
        }
        if self.timer.is_none() {
            self.timer = Some(now + self.rto());
        }
    }

    fn fin_out(&mut self) {
        self.fin_sent = true;
        self.state = match self.state {
            State::Established => State::FinWait1,
            State::CloseWait => State::LastAck,
            s => s,
        };
    }
}
