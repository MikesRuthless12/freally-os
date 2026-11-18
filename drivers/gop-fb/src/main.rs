// SPDX-License-Identifier: Apache-2.0
//! drv-gop-fb (TASK-042): the framebuffer the firmware set up (GOP), as a
//! write-combining VMO. devmgr hands it exactly the framebuffer's range;
//! stage (Phase 3) maps the VMO it offers and draws.
//!
//! - `info`: the VMO (a duplicate, handle 0), width, height, stride, format
//!   and the EDID's size.
//! - `fill`: one colour across the whole screen (the e2e spec's check).
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::{FErr, cache, perm, rights, signal, well_known};
use fstd::handle::Handle;
use fstd::ipc::{Channel, Client};
use fstd::time::{Duration, Instant};
use fwire::HandleRef;
use fwire::proto::conductor as c;
use fwire::proto::devmgr as d;
use fwire::proto::display as disp;

fstd::program!(run);

struct Fb {
    vmo: Handle,
    pixels: *mut u32,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
    edid_size: u32,
}

impl Fb {
    fn fill(&self, rgb: u32) {
        // BGRX keeps 0xRRGGBB as is in a little-endian u32; RGBX swaps red and blue.
        let px = if self.format == 0 { rgb } else { (rgb & 0xFF00) | ((rgb >> 16) & 0xFF) | ((rgb & 0xFF) << 16) };
        for y in 0..self.height as usize {
            for x in 0..self.width as usize {
                // SAFETY: inside the mapped framebuffer (height × stride pixels).
                unsafe { core::ptr::write_volatile(self.pixels.add(y * self.stride as usize + x), px) };
            }
        }
    }
}

fn value(r: &d::Resources, name: &str) -> u64 {
    r.values.iter().find(|(k, _)| k == name).map_or(0, |(_, v)| *v)
}

fn open() -> Result<Fb, String> {
    let devmgr = fstd::service::connect("devmgr").map_err(|e| alloc::format!("devmgr: {e:?}"))?;
    let mut client = Client::new(devmgr);
    let (r, mut handles) = client
        .call::<_, d::Resources>(d::RESOURCES, &(), Vec::new(), Instant::after(Duration::from_secs(2)))
        .map_err(|e| alloc::format!("devmgr's resources: {e:?}"))?;
    let i = r.handles.iter().find(|(n, _)| n == "framebuffer").map(|(_, h)| h.0 as usize).ok_or("no framebuffer")?;
    let res = handles.swap_remove(i);
    let (base, size) = (value(&r, "base"), value(&r, "size"));
    let len = size.next_multiple_of(4096);
    let vmo = fstd::sys::vmo_create_physical(res.raw(), base, len, cache::WRITE_COMBINING)
        .map_err(|e| alloc::format!("vmo_create_physical: {}", e.describe()))?;
    let vmo = Handle::from_raw(vmo).ok_or("no VMO")?;
    let at = fstd::sys::vm_map(well_known::SELF_ASPACE, vmo.raw(), 0, len, 0, perm::READ | perm::WRITE)
        .map_err(|e| alloc::format!("vm_map: {}", e.describe()))?;
    Ok(Fb {
        vmo,
        pixels: at as *mut u32,
        width: value(&r, "width") as u32,
        height: value(&r, "height") as u32,
        stride: value(&r, "stride") as u32,
        format: value(&r, "format") as u32,
        edid_size: value(&r, "edid_size") as u32,
    })
}

fn run(_bootstrap: u32) -> i64 {
    if fstd::service::start().is_err() {
        return 1;
    }
    let _ = fstd::log::connect();
    let fb = match open() {
        Ok(f) => f,
        Err(e) => {
            fstd::error!("drv-gop-fb: {e}");
            return 1;
        }
    };
    fstd::info!("drv-gop-fb: {}x{}, stride {}, write-combining", fb.width, fb.height, fb.stride);
    let Ok((listen, give)) = Channel::pair() else { return 1 };
    if fstd::service::register(give).is_err() {
        return 1;
    }
    let mut clients: Vec<Channel> = Vec::new();
    loop {
        let mut items = alloc::vec![[listen.0.raw(), signal::READABLE]];
        items.extend(clients.iter().map(|ch| [ch.0.raw(), signal::READABLE | signal::PEER_CLOSED]));
        match fstd::sys::wait_many(&items, u64::MAX) {
            Ok(0) => {
                if let Ok(m) = listen.recv(0)
                    && let Ok((h, _)) = fwire::parse(&m.bytes)
                    && h.ordinal == c::INCOMING
                    && let Some(ch) = m.handles.into_iter().next()
                {
                    clients.push(Channel(ch));
                }
            }
            Ok(k) => {
                if !serve(&fb, &clients[k - 1]) {
                    clients.remove(k - 1);
                }
            }
            Err(_) => fstd::time::sleep(Duration::from_millis(10)),
        }
    }
}

fn serve(fb: &Fb, ch: &Channel) -> bool {
    loop {
        let mut bytes = alloc::vec![0u8; 256];
        let mut hs = [0u32; 2];
        match fstd::sys::channel_read(ch.0.raw(), &mut bytes, &mut hs) {
            Ok((n, _)) if fstd::inventory::seen(ch, &bytes[..n]) => {}
            Ok((n, _)) => {
                let Ok((h, p)) = fwire::parse(&bytes[..n]) else { continue };
                let (reply, handles) = match h.ordinal {
                    disp::INFO => match fb.vmo.duplicate(rights::READ | rights::WRITE | rights::MAP | rights::TRANSFER)
                    {
                        Ok(v) => (
                            fwire::reply(
                                h.txid,
                                h.ordinal,
                                &disp::Info {
                                    vmo: HandleRef(0),
                                    width: fb.width,
                                    height: fb.height,
                                    stride: fb.stride,
                                    format: fb.format,
                                    edid_size: fb.edid_size,
                                },
                            ),
                            alloc::vec![v],
                        ),
                        Err(e) => (fwire::error_reply(h.txid, h.ordinal, e as i32, "no duplicate"), Vec::new()),
                    },
                    disp::FILL => match fwire::from_payload::<disp::Fill>(p) {
                        Ok(f) => {
                            fb.fill(f.rgb);
                            (fwire::reply(h.txid, h.ordinal, &()), Vec::new())
                        }
                        Err(e) => {
                            (fwire::error_reply(h.txid, h.ordinal, FErr::InvalidArgs as i32, e.describe()), Vec::new())
                        }
                    },
                    _ => (fwire::error_reply(h.txid, h.ordinal, FErr::NotSupported as i32, "not served"), Vec::new()),
                };
                if let Ok(m) = reply {
                    let _ = ch.send(&m, handles);
                }
            }
            Err(FErr::ShouldWait) => return true,
            Err(_) => return false,
        }
    }
}
