// SPDX-License-Identifier: Apache-2.0
//! The trust roots compiled into every image (TASK-179, TASK-181): the
//! certificates (DER) a server's chain must end at. None yet: the owner picks
//! the update host first, and its root (and nothing wider) goes here then. A
//! dev or test image may be handed one more root at run time (the shell's
//! `fetch --root`); a release image trusts only these.
pub const ROOTS: &[&[u8]] = &[];
