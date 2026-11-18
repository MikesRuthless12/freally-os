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

    /// Takes the next message, waiting until `deadline_ns` for one.
    pub fn recv(&self, deadline_ns: u64) -> Result<Message, FErr> {
        let mut bytes = vec![0u8; 512];
        let mut handles = [0u32; 16];
        loop {
            match sys::channel_read(self.0.raw(), &mut bytes, &mut handles) {
                Ok((n, nh)) => {
                    bytes.truncate(n);
                    let hs = handles[..nh].iter().filter_map(|&v| Handle::from_raw(v)).collect();
                    return Ok(Message { bytes, handles: hs });
                }
                Err(FErr::ShouldWait) => {
                    let seen =
                        self.0.wait(fcore_abi::signal::READABLE | fcore_abi::signal::PEER_CLOSED, deadline_ns)?;
                    if seen & fcore_abi::signal::READABLE == 0 {
                        return Err(FErr::PeerClosed);
                    }
                }
                // The kernel says how big: grow and read again.
                Err(FErr::BufferTooSmall) => bytes.resize(bytes.len() * 4, 0),
                Err(e) => return Err(e),
            }
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

fn check_reply(h: &Header, txid: u32, ordinal: u32) -> Result<(), CallError> {
    if !h.is_reply() || h.txid != txid || h.ordinal != ordinal {
        return Err(CallError::Wire(WireError::BadTag));
    }
    Ok(())
}
