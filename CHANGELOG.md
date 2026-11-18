# Changelog

All notable changes to Freally OS. Versions follow the ladder: `vX.Y.Z`, `-beta.N`, `-rc.N`.

## `v0.4.0`: First Light, the desktop (2026-11-18)

The first four rungs, closed together: `v0.1.0` to `v0.3.0` were reached on the way and are recorded here, and `v0.4.0` is the first tag. Nothing is downloadable, and nothing will be before `v1.0.0`.

### `v0.1.0`: the foundation

- Freally OS's own UEFI bootloader, `freally-boot`: it reads the signed system image, checks its Ed25519 signature, every block through a Merkle tree and every file's hash, and says on screen why it refused one that fails. Its text is drawn in Noto Sans by its own TrueType rasteriser.
- The image format, `fsi` (`lib/fsi`), and `cargo xtask pack`, `keygen` and `sign`.
- Every crate is in this repository: a gate refuses any crate from outside it. Others refuse AI code, the denylist's words, a missing SPDX line, and the private planning documents.
- `cargo xtask ci` (every gate, logged), `ci-linux` (the same in a Docker container, with reproducible builds), the release path, and `showcase` (films of the real OS).
- The CDP leg: specs drive the real VM over the Chrome DevTools Protocol, one per feature in `e2e/os/coverage.json`.

### `v0.2.0`: Freally Core, the kernel

- The handoff from UEFI, the kernel's own page tables and a direct map of physical memory, a heap, and its test harness (every test run inside the kernel).
- Interrupts and time: the local APIC (xAPIC and x2APIC), the I/O APIC, HPET and the TSC-deadline timer.
- Objects and handles with rights, address spaces with PCIDs, W^X everywhere, SMEP and SMAP; threads, a scheduler with four classes, system calls, channels with handoff, events and waits.
- Conductor, process 1, started from the signed image; fault reports for crashed programs, and a last-panic page read on the next boot.
- The docs site, in 18 languages, tested over CDP in headless Chrome.

### `v0.3.0`: services, drivers and the seal

- Every processor core: the other cores started through a trampoline, a scheduler per core with work stealing, TLB shootdowns, and K3 when every core is online.
- Manifests for every service, conductor's ELF loader, grants, a directory, and restarts with backoff.
- The logger, devmgr (ACPI and PCIe), and drivers for the firmware's screen, the PS/2 keyboard and mouse, and the serial ports.
- A/B slots with trial boots: an update that doesn't start falls back to the last good one on its own.
- `xtask validate`: conductor re-checks each file it maps; each flavour proven to hold only what it should.

### `v0.4.0`: First Light, the desktop

- Tokens from the fleet's UI kit, the hardware set's renders and the OS's own (the Studio wall, the Rack, window strips, icons), and Noto Sans and Noto Sans Mono throughout.
- Freally UI: the widget tree, layout, text, themes, scale, and an accessibility tree for every window.
- Stage, the compositor: windows, damage, focus, shortcuts, light and dark, and the fleet cursor (a ring and its dot).
- The desktop: the Rack with its app keys and clock, and the launcher (Super).
- Freally Terminal and Freally Shell, with `help`, `ps`, `boot`, `log`, `mem`, `uptime` and more.
- Monitor (Super+M): every process with what it's for, the boot stage by stage, faults and the log.
- `cdpd`, the test door's other half: the DOM and accessibility trees, the guard (any error or fault fails a spec), and the inventory (every control worked by a spec).
- The boot gate, `cargo xtask boot-check`: five boots to the desktop, median 539 ms (the gate is 3,000), 9 processes, each described, 37 MiB in use.
