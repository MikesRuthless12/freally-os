// SPDX-License-Identifier: Apache-2.0
//! conductor, PID 1 (TASK-038): reads every manifest in the image, starts the
//! boot services in dependency order, restarts any that end (with backoff),
//! serves the directory, and keeps the fault reports the kernel files.
#![cfg_attr(target_os = "none", no_std, no_main)]

extern crate alloc;

mod bootctl;
#[cfg(feature = "flavour-e2e")]
mod door;
#[cfg(feature = "flavour-e2e")]
mod door_stage;
mod faults;
mod image;
mod loader;
mod services;

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::FErr;
use fstd::time::{Duration, Instant};
use services::{Conductor, Wake};

fstd::program!(run);

/// The image's manifests, parsed and checked as a set.
fn manifests(img: &image::Image) -> Result<(Vec<fsvc::Manifest>, Vec<String>), String> {
    let mut out = Vec::new();
    for path in img.paths()? {
        if path.starts_with("/system/manifests/") && path.ends_with(".svc") {
            let text = core::str::from_utf8(img.file(&path)?).map_err(|_| alloc::format!("{path} isn't UTF-8"))?;
            let m = fsvc::parse(text).map_err(|e| alloc::format!("{path}: {e}"))?;
            if m.name != "conductor" {
                out.push(m);
            }
        }
    }
    let order = fsvc::check_set(&out)?;
    Ok((out, order))
}

fn run(_bootstrap: u32) -> i64 {
    fstd::info!("conductor: started");
    let img = match image::Image::open() {
        Ok(i) => i,
        Err(e) => {
            fstd::error!("conductor: can't read the system image: {}", e.describe());
            return 1;
        }
    };
    let (set, order) = match manifests(&img) {
        Ok(x) => x,
        Err(e) => {
            fstd::error!("conductor: the manifests are wrong: {e}");
            return 1;
        }
    };
    fstd::info!("conductor: {} services in Freally OS {}", set.len(), img.version);
    let mut c = Conductor::new(img, set);
    c.start_boot(&order);
    #[cfg(feature = "flavour-e2e")]
    let mut door = door::Door::open();
    loop {
        c.start_due();
        #[allow(unused_mut)]
        let (mut items, what) = c.wait_items();
        // The door's port: an answer starts as soon as its command arrives.
        #[cfg(feature = "flavour-e2e")]
        items.extend(door.wait_item());
        let mut deadline = c.next_start().unwrap_or(u64::MAX).min(c.boot.deadline().unwrap_or(u64::MAX));
        if cfg!(feature = "flavour-e2e") {
            deadline = deadline.min(Instant::after(Duration::from_millis(20)));
        }
        match fstd::sys::wait_many(&items, deadline) {
            Ok(k) => match what.get(k).copied() {
                Some(Wake::Faults) => {
                    if let Err(e) = c.faults.take_now() {
                        fstd::error!("conductor: can't read fault reports: {}", e.describe());
                    }
                }
                Some(Wake::Exited(i)) => c.exited(i),
                Some(Wake::Request(i)) => c.request(i),
                None => {}
            },
            Err(FErr::TimedOut) => {}
            Err(e) => {
                fstd::error!("conductor: waiting failed: {}", e.describe());
                fstd::time::sleep(Duration::from_millis(100));
            }
        }
        c.boot.tick();
        #[cfg(feature = "flavour-e2e")]
        door.poll(&mut c);
    }
}
