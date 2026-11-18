# Freally OS

**A fast, private operating system where every line of code is written in-house.**

Press power and the desktop is up before you've sat down. Nothing runs that you didn't start, nothing phones home, and nothing updates without asking. No online account, no ads, no telemetry, and no AI inside.

Freally OS is written from the first instruction by Mike, the maker of the Freally music software. That covers its own UEFI bootloader, its own microkernel (Freally Core), its own drivers, its own compositor and toolkit, and every app. It's written in Rust, with Blender-rendered visuals in the Freally studio-hardware style.

> **Status: Phase 0 of the road to `v1.0.0`.** Nothing is downloadable yet, and nothing will be before `v1.0.0`. This repository is where the work happens in the open.

## What makes it different

- **Every line is ours.** The only outside code compiled in is the Rust toolchain's own `core`, `alloc` and `compiler_builtins` ([`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md)). A CI gate fails the build if anything else appears.
- **Sealed and signed.** Every system image carries an Ed25519 signature and a SHA-256 Merkle root, and Freally's own bootloader verifies both before anything runs ([`SECURITY.md`](SECURITY.md)).
- **Fast is a number.** Boot time, process count and memory are CI gates, not goals.
- **Reproducible.** Two clean builds of the same commit produce the same image hash, so anyone can check that an official image came from this source.

## Building

You need Ubuntu under WSL2 (or Linux) with KVM, QEMU and OVMF, and `rustup`. The repository pins its own Rust nightly in `rust-toolchain.toml`.

```sh
sudo apt install qemu-system-x86 ovmf build-essential
cargo xtask doctor            # checks KVM, QEMU, OVMF and the toolchain
cargo xtask keygen --dev      # a dev signing key in ~/.freally-keys/ (never in the repo)
cargo xtask run --flavour dev # builds, signs and boots it in QEMU
cargo xtask ci                # every gate, reported one by one
```

Always build through `cargo xtask`: the bootloader, the kernel and user space each need their own target and flags.

## Licence

The code is licensed under the [Apache License 2.0](LICENSE).

The Freally name and logo, the Freally hardware-set renders, the UI kit and Freally OS's own renders are **All Rights Reserved** ([`TRADEMARKS.md`](TRADEMARKS.md), [`assets/LICENSE-BRAND.md`](assets/LICENSE-BRAND.md)). A fork is welcome under another name, with its own assets and its own signing key.

## Contributing

Issues, hardware reports and security reports are welcome. Pull requests aren't merged: only Mike changes Freally OS ([`CONTRIBUTING.md`](CONTRIBUTING.md) says why).

Freally OS is built openly with Claude Code as Mike's coding agent. The OS itself contains no AI.

Contact: freallyproducts@gmail.com
