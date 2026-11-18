# Third-party notices

Every line of Freally OS is written in-house. The only outside code compiled into it is the Rust toolchain's own libraries, listed below, until Freally's own language replaces them. Outside **data** (fonts now; device firmware later) may be MIT, Apache-2.0, CC-BY-4.0 or, for fonts, OFL-1.1, and is always 100% free.

| Component | Where it's used | Licence | Source |
|---|---|---|---|
| Rust `core` | every crate | MIT OR Apache-2.0 | <https://github.com/rust-lang/rust> (`library/core`) |
| Rust `alloc` | crates that allocate | MIT OR Apache-2.0 | <https://github.com/rust-lang/rust> (`library/alloc`) |
| Rust `compiler_builtins` | every target (memcpy, 128-bit arithmetic, …) | MIT OR Apache-2.0 | <https://github.com/rust-lang/compiler-builtins> |

The host tools (`xtask`, `cdp-host`, `cdp-runner`) also use Rust's `std` (MIT OR Apache-2.0). They run on the build machine and are never part of a Freally OS image.

## Fonts, loaded as data (owner, 2026-11-17)

Freally OS is set in **Noto**, everywhere from the boot screen on. The fonts are data, never code: every line that reads and draws them is Freally's own (`lib/ffont`).

| Font | Where it's used | Licence | Source |
|---|---|---|---|
| Noto Sans (Regular, SemiBold) | the interface, the boot and integrity screens | SIL Open Font License 1.1 | <https://github.com/notofonts/latin-greek-cyrillic> |
| Noto Sans Mono (Regular) | Freally Terminal, code | SIL Open Font License 1.1 | <https://github.com/notofonts/latin-greek-cyrillic> |
| `assets/fonts/noto/boot-atlas.fatl` | the boot screen's glyphs, pre-rendered from Noto Sans by `cargo xtask atlas` | SIL Open Font License 1.1 (as the fonts it comes from) | this repository |

The licence is in [`assets/fonts/noto/OFL.txt`](assets/fonts/noto/OFL.txt), copyright 2022 The Noto Project Authors.

**Device firmware** (from Wi-Fi and the GPU phase) will be loaded onto devices as data and listed here, each with its licence and source. None is used yet.

**Tools, never linked in:** QEMU, OVMF, Docker, Google Chrome, Node.js, ffmpeg and Blender run on the build machine only.

The no-third-party gate (`cargo xtask ci`) fails if `Cargo.lock` ever lists a package from outside this workspace.
