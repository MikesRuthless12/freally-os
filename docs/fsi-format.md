# The Freally system image (`.fsi`), format 1

<!-- SPDX-License-Identifier: Apache-2.0 -->

A system image is one file per slot (`\FREALLY\SYSTEM_A.FSI`, `SYSTEM_B.FSI`). The bootloader verifies all of it before anything in it runs. It's read by `lib/fsi` and written by `cargo xtask pack` and `cargo xtask sign`.

All integers are little-endian. A block is 4,096 bytes.

## Layout

| Blocks | What |
|---|---|
| 0 | The header (exactly 4,096 bytes) |
| 1 ..= `data_blocks` | The data region: the TOC first (at block 1), then each file at a block boundary. A short last block is zero-padded. |
| after the data | The Merkle tree, level 0 (the leaves) first, then each level up to the root. Not block-aligned at its end. |

`image_len = tree_offset + tree_len`, and the file is exactly `image_len` bytes long.

## The header (block 0)

| Offset | Size | Field | Meaning |
|---|---|---|---|
| 0 | 8 | `magic` | `FRLYSI01` |
| 8 | 2 | `format` | `1` |
| 10 | 2 | `flavour` | 0 release, 1 dev, 2 e2e |
| 12 | 32 | `version` | e.g. `0.1.0`, UTF-8, NUL-padded |
| 44 | 20 | `build_id` | the git commit (SHA-1) it was built from |
| 64 | 8 | `created_unix` | the commit's time (`SOURCE_DATE_EPOCH`), never the build machine's clock |
| 72 | 8 | `image_len` | total bytes, header and tree included |
| 80 | 8 | `data_blocks` | blocks covered by the tree, from block 1; at least 1 |
| 88 | 8 | `toc_offset` | byte offset of the TOC; `4096` |
| 96 | 8 | `toc_len` | `160 × entries` |
| 104 | 8 | `tree_offset` | `(1 + data_blocks) × 4096` |
| 112 | 8 | `tree_len` | `32 × nodes(data_blocks)` |
| 120 | 32 | `merkle_root` | SHA-256 |
| 152 | 8 | `key_id` | the first 8 bytes of SHA-256(the signer's Ed25519 public key) |
| 160 | 3,872 | reserved | zero |
| 4032 | 64 | `signature` | Ed25519 over bytes `[0, 4032)` |

## A TOC entry (160 bytes)

| Offset | Size | Field | Meaning |
|---|---|---|---|
| 0 | 96 | `path` | `/system/...`, UTF-8, NUL-padded, starts with `/` |
| 96 | 2 | `kind` | 1 Kernel, 2 Conductor, 3 Service, 4 Driver, 5 App, 6 Manifest, 7 Asset, 8 Font, 9 Data |
| 98 | 2 | `flags` | bit 0 dev only, bit 1 e2e only; the rest zero |
| 100 | 4 | padding | zero |
| 104 | 8 | `offset` | byte offset of the file in the image, block-aligned, inside the data region |
| 112 | 8 | `len` | its length in bytes |
| 120 | 32 | `sha256` | SHA-256 of the file's bytes |
| 152 | 8 | reserved | zero |

Entries are sorted by `path` (byte order), with no duplicates, so a path can be found by binary search.

## The Merkle tree

- **Leaves:** the SHA-256 of each data block (block 1 to `data_blocks`).
- **Parents:** SHA-256(left ‖ right). An odd node at the end of a level is promoted to the next level unchanged.
- **Levels:** `n₀ = data_blocks`, `nᵢ₊₁ = ⌈nᵢ / 2⌉`, until a level of one node, the root. `nodes(n)` is the sum of every level's size (for one block, it's 1: the leaf is the root).
- The root is in the header, the signature covers the header, so the signature covers every block.

## Verifying (the bootloader's order)

1. The file is at least one block long; `magic`, `format` and `flavour` are known.
2. `key_id` names a key this bootloader embeds, and the signature over `[0, 4032)` verifies with it. **Nothing in the header is trusted before this step.**
3. The reserved bytes are zero, and the layout adds up: `image_len` equals the file's length, `tree_offset`, `tree_len` and the TOC sit where this page says.
4. Every leaf equals the SHA-256 of its block; every parent equals the hash of its children; the top equals `merkle_root`.
5. Every TOC entry is well formed, sorted, inside the data region, and its `sha256` matches its bytes.

Any failure refuses the image and shows the integrity screen. No flag, variable or boot option skips a step.
