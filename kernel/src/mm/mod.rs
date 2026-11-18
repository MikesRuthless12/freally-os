// SPDX-License-Identifier: Apache-2.0
//! Memory: physical frames, page tables, the kernel heap, address spaces, VMOs,
//! and copies to and from user memory.

pub mod aspace;
pub mod frames;
pub mod heap;
pub mod paging;
pub mod user;
pub mod vmo;
