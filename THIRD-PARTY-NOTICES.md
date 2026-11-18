# Third-party notices

Every line of Freally OS is written in-house. The only outside code compiled into it is the Rust toolchain's own libraries, listed below, until Freally's own language replaces them.

| Component | Where it's used | Licence | Source |
|---|---|---|---|
| Rust `core` | every crate | MIT OR Apache-2.0 | <https://github.com/rust-lang/rust> (`library/core`) |
| Rust `alloc` | crates that allocate | MIT OR Apache-2.0 | <https://github.com/rust-lang/rust> (`library/alloc`) |
| Rust `compiler_builtins` | every target (memcpy, 128-bit arithmetic, …) | MIT OR Apache-2.0 | <https://github.com/rust-lang/compiler-builtins> |

The host tools (`xtask`, `cdp-host`, `cdp-runner`) also use Rust's `std` (MIT OR Apache-2.0). They run on the build machine and are never part of a Freally OS image.

**Device firmware** (from Wi-Fi and the GPU phase) will be loaded onto devices as data and listed here, each with its licence and source. None is used yet.

**Tools, never linked in:** QEMU, OVMF, Docker, Google Chrome, Node.js, ffmpeg and Blender run on the build machine only.

The no-third-party gate (`cargo xtask ci`) fails if `Cargo.lock` ever lists a package from outside this workspace.
