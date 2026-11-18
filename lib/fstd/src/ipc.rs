// SPDX-License-Identifier: Apache-2.0
//! Channels and the service-call client (fwire requests and replies).

use crate::handle::Handle;
use crate::sys;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use fcore_abi::FErr;
use fwire::{Header, Wire, WireError};

pub struct Channel(pub Handle);

/// A received message.
pub struct Message {
    pub bytes: Vec<u8>,
    pub handles: Vec<Handle>,
}

impl Channel {
    pub fn pair() -> Result<(Channel, Channel), FErr> {
        let (a, b) = sys::channel_create()?;
        Ok((Channel(Handle::from_raw(a).ok_or(FErr::Internal)?), Channel(Handle::from_raw(b).ok_or(FErr::Internal)?)))
    }

    /// Sends `bytes`, moving `handles` into the message.
    pub fn send(&self, bytes: &[u8], handles: Vec<Handle>) -> Result<(), FErr> {
        let raw: Vec<u32> = handles.iter().map(Handle::raw).collect();
        sys::channel_write(self.0.raw(), bytes, &raw)?;
        // They belong to the message now.
        handles.into_iter().for_each(|h| {
            h.into_raw();
        });
        Ok(())
    }

    /// Takes the next message, waiting until `deadline_ns` for one (a
    /// deadline already past gives `TimedOut` on an empty channel: use
    /// `try_recv` to look without waiting).
    pub fn recv(&self, deadline_ns: u64) -> Result<Message, FErr> {
        loop {
            match self.try_recv() {
                Err(FErr::ShouldWait) => {
                    let seen =
                        self.0.wait(fcore_abi::signal::READABLE | fcore_abi::signal::PEER_CLOSED, deadline_ns)?;
                    if seen & fcore_abi::signal::READABLE == 0 {
                        return Err(FErr::PeerClosed);
                    }
                }
                other => return other,
            }
        }
    }

    /// The next message if one is waiting, never waiting: `ShouldWait` when
    /// there's none yet, `PeerClosed` when none will come.
    pub fn try_recv(&self) -> Result<Message, FErr> {
        loop {
            let (bytes, hs) = read_whole(|b, h| sys::channel_read(self.0.raw(), b, h))?;
            if crate::inventory::seen(self, &bytes) {
                continue;
            }
            let handles = hs.iter().filter_map(|&v| Handle::from_raw(v)).collect();
            return Ok(Message { bytes, handles });
        }
    }
}

/// Reads one whole message with `read` (a `channel_read`): its bytes and
/// raw handles. A buffer too small grows (to the kernel's 64 KiB) and reads
/// again, so a large message is taken off the channel, never left at its head
/// to fail every read after it.
pub fn read_whole(
    mut read: impl FnMut(&mut [u8], &mut [u32]) -> Result<(usize, usize), FErr>,
) -> Result<(Vec<u8>, Vec<u32>), FErr> {
    let mut bytes = vec![0u8; 512];
    let mut handles = [0u32; 16];
    loop {
        match read(&mut bytes, &mut handles) {
            Ok((n, nh)) => {
                bytes.truncate(n);
                return Ok((bytes, handles[..nh].to_vec()));
            }
            Err(FErr::BufferTooSmall) if bytes.len() < 1 << 16 => bytes.resize(bytes.len() * 4, 0),
            Err(e) => return Err(e),
        }
    }
}

/// Why a call failed.
#[derive(Debug, PartialEq, Eq)]
pub enum CallError {
    /// The kernel refused (a bad handle, the peer gone, a timeout...).
    Sys(FErr),
    /// The reply couldn't be read.
    Wire(WireError),
    /// The service answered with an error.
    Remote(i32, String),
}

impl From<FErr> for CallError {
    fn from(e: FErr) -> CallError {
        CallError::Sys(e)
    }
}
impl From<WireError> for CallError {
    fn from(e: WireError) -> CallError {
        CallError::Wire(e)
    }
}

/// A client for one service: numbers its requests and checks each reply.
pub struct Client {
    pub channel: Channel,
    next_txid: u32,
}

impl Client {
    pub fn new(channel: Channel) -> Client {
        Client { channel, next_txid: 1 }
    }

    /// Calls `ordinal` with `req`, moving `handles` along; returns the reply
    /// body and any handles it carried.
    pub fn call<Req: Wire, Rep: Wire>(
        &mut self,
        ordinal: u32,
        req: &Req,
        handles: Vec<Handle>,
        deadline_ns: u64,
    ) -> Result<(Rep, Vec<Handle>), CallError> {
        let txid = self.next_txid;
        self.next_txid = self.next_txid.wrapping_add(1).max(1);
        let msg = fwire::request(txid, ordinal, req)?;
        let raw: Vec<u32> = handles.iter().map(Handle::raw).collect();
        let mut reply = vec![0u8; fwire::HEADER_LEN + 4096];
        let mut rh = [0u32; 16];
        let (n, nh) = loop {
            match sys::channel_call(self.channel.0.raw(), &msg, &raw, &mut reply, &mut rh, deadline_ns) {
                Ok(r) => break r,
                Err(FErr::BufferTooSmall) if reply.len() < 65_536 => reply.resize(65_536, 0),
                Err(e) => return Err(e.into()),
            }
        };
        handles.into_iter().for_each(|h| {
            h.into_raw();
        });
        let got: Vec<Handle> = rh[..nh].iter().filter_map(|&v| Handle::from_raw(v)).collect();
        let (h, payload) = fwire::parse(&reply[..n])?;
        check_reply(&h, txid, ordinal)?;
        if h.is_error() {
            let (code, text) = fwire::error_of(payload)?;
            return Err(CallError::Remote(code, text));
        }
        Ok((fwire::from_payload(payload)?, got))
    }
}

impl Client {
    /// As `call` (no handles either way), but while it waits it also watches
    /// `others`, channels, running `serve(i)` whenever a message waits on
    /// `others[i]`: for a caller others keep asking meanwhile (conductor's
    /// test door asks cdpd, which asks conductor for the services, and so may
    /// any app cdpd asks in turn).
    pub fn call_serving<Req: Wire, Rep: Wire>(
        &mut self,
        ordinal: u32,
        req: &Req,
        deadline_ns: u64,
        others: &[u32],
        mut serve: impl FnMut(usize),
    ) -> Result<Rep, CallError> {
        use fcore_abi::signal;
        let txid = self.next_txid;
        self.next_txid = self.next_txid.wrapping_add(1).max(1);
        self.channel.send(&fwire::request(txid, ordinal, req)?, Vec::new())?;
        let mut items = alloc::vec![[self.channel.0.raw(), signal::READABLE | signal::PEER_CLOSED]];
        items.extend(others.iter().map(|&o| [o, signal::READABLE]));
        loop {
            match sys::wait_many(&items, deadline_ns)? {
                0 => {
                    let m = match self.channel.try_recv() {
                        Ok(m) => m,
                        Err(FErr::ShouldWait) => continue,
                        Err(e) => return Err(e.into()),
                    };
                    let (h, payload) = fwire::parse(&m.bytes)?;
                    if h.txid != txid {
                        continue; // a late reply to an earlier call
                    }
                    check_reply(&h, txid, ordinal)?;
                    if h.is_error() {
                        let (code, text) = fwire::error_of(payload)?;
                        return Err(CallError::Remote(code, text));
                    }
                    return Ok(fwire::from_payload(payload)?);
                }
                i => serve(i - 1),
            }
        }
    }
}

fn check_reply(h: &Header, txid: u32, ordinal: u32) -> Result<(), CallError> {
    if !h.is_reply() || h.txid != txid || h.ordinal != ordinal {
        return Err(CallError::Wire(WireError::BadTag));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_large_message_is_read_whole() {
        // A channel holding a 3,000-byte message with 2 handles: a read with
        // less room fails without taking it, as the kernel's does.
        let read = |b: &mut [u8], h: &mut [u32]| {
            if b.len() < 3000 {
                return Err(FErr::BufferTooSmall);
            }
            b[..3000].fill(7);
            h[..2].copy_from_slice(&[11, 12]);
            Ok((3000, 2))
        };
        let (bytes, hs) = read_whole(read).unwrap();
        assert_eq!((bytes.len(), hs), (3000, vec![11, 12]));
        // Past the kernel's 64 KiB nothing fits: an error, not a loop.
        assert_eq!(read_whole(|_, _| Err(FErr::BufferTooSmall)), Err(FErr::BufferTooSmall));
    }
}
