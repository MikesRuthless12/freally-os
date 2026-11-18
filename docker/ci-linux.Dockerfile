# SPDX-License-Identifier: Apache-2.0
# Freally OS's Linux CI leg (closeout step 5): a clean Ubuntu, the pinned Rust
# toolchain, QEMU and OVMF. `cargo xtask ci-linux` clones the commit into it
# fresh, runs every gate under QEMU's TCG (no KVM here), and compares the build's
# hashes with the WSL build's. The container never holds a signing key.
FROM ubuntu:26.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update -qq \
 && apt-get install -y -qq --no-install-recommends \
      build-essential ca-certificates curl git qemu-system-x86 ovmf \
 && rm -rf /var/lib/apt/lists/*

# rustup, then exactly the toolchain the repository pins.
ENV RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo PATH=/root/.cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none --profile minimal
COPY rust-toolchain.toml /tmp/toolchain/rust-toolchain.toml
RUN cd /tmp/toolchain && rustup show active-toolchain || rustup toolchain install \
 && cd /tmp/toolchain && rustup show

# The same build-cache path as WSL, so nothing path-dependent can differ.
ENV CARGO_TARGET_DIR=/root/.freally-target
WORKDIR /work
