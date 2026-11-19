// SPDX-License-Identifier: Apache-2.0
//! Transactions (§ 13): every change since the last commit is one
//! transaction; it commits on `fsync`, at unmount, 5 s after its first
//! change, or once it holds 4,096 dirty blocks. Unmount's commit sets the
//! superblock's *clean* flag; the first commit after mount clears it, so a
//! volume found not clean wasn't unmounted (part 3 runs `ffsck` then).

use crate::FsError;
use crate::store::Store;
use crate::superblock::CLEAN;
use crate::volume::Volume;

/// A transaction this old commits (ns).
pub const COMMIT_AFTER_NS: u64 = 5_000_000_000;
/// A transaction this large commits (blocks).
pub const COMMIT_AT_BLOCKS: usize = 4096;

impl<S: Store> Volume<S> {
    /// The time the caller says it is (Unix ns), for the inodes' times.
    pub fn set_time(&mut self, now_ns: u64) {
        self.now = now_ns;
    }

    /// Blocks this transaction holds in memory: nodes and data.
    pub fn dirty_blocks(&self) -> usize {
        self.dirty.len() + self.data.len()
    }

    /// Whether the transaction should commit now (`now_ns` monotonic): it's
    /// large, or 5 s have passed since it was first seen holding a change.
    pub fn should_commit(&mut self, now_ns: u64) -> bool {
        let n = self.dirty_blocks();
        if n == 0 {
            return false;
        }
        let since = *self.first_change.get_or_insert(now_ns);
        n >= COMMIT_AT_BLOCKS || now_ns.saturating_sub(since) >= COMMIT_AFTER_NS
    }

    /// Makes everything so far durable.
    pub fn fsync(&mut self) -> Result<(), FsError> {
        self.commit()
    }

    /// Commits with the *clean* flag and gives the store back.
    pub fn unmount(mut self) -> Result<S, FsError> {
        self.commit_with(CLEAN)?;
        Ok(self.into_store())
    }

    /// Whether the generation mounted was unmounted cleanly.
    pub fn was_clean(&self) -> bool {
        self.superblock().flags & CLEAN != 0
    }
}
