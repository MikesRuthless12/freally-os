// SPDX-License-Identifier: Apache-2.0
//! Building and signing an image (host side: `cargo xtask pack` and `sign`).
//! Deterministic: the same inputs give the same bytes, on any machine.

use crate::merkle::{leaf, parent};
use crate::toc::TocEntry;
use crate::{BLOCK, FsiError, HEADER_LEN, Header, Kind, SIGNED_LEN, TOC_ENTRY_LEN, key_id, tree_nodes};
use alloc::string::String;
use alloc::vec::Vec;
use fcrypto::{ed25519, sha2::sha256};

/// The files and facts an image is built from.
#[derive(Clone, Debug)]
pub struct ImageBuilder {
    pub flavour: u16,
    pub version: String,
    pub build_id: [u8; 20],
    pub created_unix: u64,
    files: Vec<(String, Kind, u16, Vec<u8>)>,
}

impl ImageBuilder {
    pub fn new(flavour: u16, version: &str, build_id: [u8; 20], created_unix: u64) -> ImageBuilder {
        ImageBuilder { flavour, version: version.into(), build_id, created_unix, files: Vec::new() }
    }

    pub fn add(&mut self, path: &str, kind: Kind, flags: u16, bytes: Vec<u8>) -> &mut Self {
        self.files.push((path.into(), kind, flags, bytes));
        self
    }

    /// The unsigned image, with `key_id` set for `public` so the signature can follow.
    pub fn build(&self, public: &[u8; 32]) -> Result<Vec<u8>, &'static str> {
        if self.version.len() > 32 {
            return Err("the version is longer than 32 bytes");
        }
        let mut files: Vec<&(String, Kind, u16, Vec<u8>)> = self.files.iter().collect();
        files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        for w in files.windows(2) {
            if w[0].0 == w[1].0 {
                return Err("two files have the same path");
            }
        }
        for f in &files {
            if !f.0.starts_with('/') || f.0.len() < 2 || f.0.len() > 96 || f.0.contains('\0') {
                return Err("a path must start with / and be at most 96 bytes");
            }
        }

        let toc_len = files.len() * TOC_ENTRY_LEN;
        let mut data: Vec<u8> = alloc::vec![0u8; toc_len.div_ceil(BLOCK).max(1) * BLOCK];
        let mut entries = Vec::with_capacity(files.len());
        for (path, kind, flags, bytes) in &files {
            let offset = BLOCK + data.len();
            data.extend_from_slice(bytes);
            let padded = data.len().div_ceil(BLOCK) * BLOCK;
            data.resize(padded, 0);
            let mut p = [0u8; 96];
            p[..path.len()].copy_from_slice(path.as_bytes());
            entries.push(TocEntry {
                path: p,
                kind: *kind as u16,
                flags: *flags,
                offset: offset as u64,
                len: bytes.len() as u64,
                sha256: sha256(bytes),
            });
        }
        for (i, e) in entries.iter().enumerate() {
            e.write(&mut data[i * TOC_ENTRY_LEN..(i + 1) * TOC_ENTRY_LEN]);
        }

        let data_blocks = (data.len() / BLOCK) as u64;
        let mut tree: Vec<[u8; 32]> = data.chunks(BLOCK).map(leaf).collect();
        let mut level_start = 0;
        let mut n = tree.len();
        while n > 1 {
            for j in 0..n.div_ceil(2) {
                let l = level_start + 2 * j;
                let node = if 2 * j + 1 < n { parent(&tree[l], &tree[l + 1]) } else { tree[l] };
                tree.push(node);
            }
            level_start += n;
            n = n.div_ceil(2);
        }
        debug_assert_eq!(tree.len() as u64, tree_nodes(data_blocks));
        let root = *tree.last().ok_or("an image has at least one block")?;

        let tree_offset = (BLOCK + data.len()) as u64;
        let tree_len = (tree.len() * 32) as u64;
        let mut version = [0u8; 32];
        version[..self.version.len()].copy_from_slice(self.version.as_bytes());
        let header = Header {
            flavour: self.flavour,
            version,
            build_id: self.build_id,
            created_unix: self.created_unix,
            image_len: tree_offset + tree_len,
            data_blocks,
            toc_offset: BLOCK as u64,
            toc_len: toc_len as u64,
            tree_offset,
            tree_len,
            merkle_root: root,
            key_id: key_id(public),
            signature: [0; 64],
        };
        let mut head = [0u8; HEADER_LEN];
        header.write(&mut head);

        let mut image = Vec::with_capacity(header.image_len as usize);
        image.extend_from_slice(&head);
        image.extend_from_slice(&data);
        for node in &tree {
            image.extend_from_slice(node);
        }
        Ok(image)
    }
}

/// Signs `image` in place with the secret `seed`. Refuses if the image's
/// `key_id` names another key.
pub fn sign(image: &mut [u8], seed: &[u8; 32]) -> Result<(), FsiError> {
    let header = Header::parse(image)?;
    let public = ed25519::public_key(seed);
    if key_id(&public) != header.key_id {
        return Err(FsiError::UnknownKey);
    }
    let sig = ed25519::sign(seed, &image[..SIGNED_LEN]);
    image[SIGNED_LEN..HEADER_LEN].copy_from_slice(&sig);
    Ok(())
}
