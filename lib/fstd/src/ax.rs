// SPDX-License-Identifier: Apache-2.0
//! An app's accessibility tree, published to stage (TASK-063): a VMO holding
//! its length and then the tree, as `publish_ax_tree` takes it.

use crate::ipc::{CallError, Client};
use crate::time::{Duration, Instant};
use fcore_abi::FErr;
use fwire::proto::ax;
use fwire::proto::stage as st;

/// Publishes `tree` for `window_id` (call it after each change it shows).
pub fn publish(stage: &mut Client, window_id: u32, tree: &ax::Tree) -> Result<(), CallError> {
    let bytes = fwire::to_payload(tree);
    let n = bytes.len() as u64;
    let vmo = crate::sys::vmo_create(n + 8)
        .ok()
        .and_then(crate::handle::Handle::from_raw)
        .ok_or(CallError::Sys(FErr::NoMemory))?;
    crate::sys::vmo_write(vmo.raw(), 0, &n.to_le_bytes())?;
    crate::sys::vmo_write(vmo.raw(), 8, &bytes)?;
    stage
        .call::<_, ()>(
            st::PUBLISH_AX_TREE,
            &st::AxTree { window_id, tree: fwire::HandleRef(0) },
            alloc::vec![vmo],
            Instant::after(Duration::from_secs(2)),
        )
        .map(|_| ())
}

/// Publishes a window's tree when it differs from the last one published
/// (a hover redraws often; its tree seldom changes).
#[derive(Default)]
pub struct Publisher {
    last: alloc::vec::Vec<u8>,
    /// The window it went to (a window made again needs it too).
    window: u32,
}

impl Publisher {
    pub fn publish(&mut self, stage: &mut Client, window_id: u32, tree: &ax::Tree) {
        let bytes = fwire::to_payload(tree);
        if self.fresh(window_id, &bytes) && publish(stage, window_id, tree).is_ok() {
            self.sent(window_id, &bytes);
        }
    }

    /// Whether this tree for this window isn't the one sent last.
    fn fresh(&self, window_id: u32, bytes: &[u8]) -> bool {
        window_id != self.window || bytes != self.last.as_slice()
    }

    fn sent(&mut self, window_id: u32, bytes: &[u8]) {
        self.window = window_id;
        self.last = bytes.to_vec();
    }
}

/// A node: `parent` its index (`ax::ROOT` for the root), bounds in surface pixels.
pub fn node(parent: u32, role: u8, name: &str, state: u32, (x, y, w, h): (i32, i32, u32, u32)) -> ax::Node {
    ax::Node {
        parent,
        role,
        name: alloc::string::String::from(name),
        state,
        value: alloc::string::String::new(),
        testid: alloc::string::String::new(),
        x,
        y,
        w,
        h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_tree_goes_to_a_new_window() {
        let mut p = Publisher::default();
        p.sent(4, b"tree");
        assert!(!p.fresh(4, b"tree"), "unchanged: not sent again");
        assert!(p.fresh(4, b"other"));
        // A window made again (a new scale): its tree is sent, unchanged or not.
        assert!(p.fresh(5, b"tree"));
    }
}
