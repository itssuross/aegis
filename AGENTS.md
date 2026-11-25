# Project Identity

- **Name:** Aegis (`itssuross/aegis`).
- **Architecture:** Bare-metal ARM64 (`aarch64-unknown-none`) hobbyist kernel.
- **Target Hardware:** QEMU `virt` machine (`-M virt -cpu cortex-a72 -m 128M -nographic -serial mon:stdio`).

## Language & Dependency Rules (STRICT)

- Use only Rust (`#![no_std]`, `#![no_main]`) and ARM64 assembly (`.S`) for source code.
- The permitted build, configuration, and instructions files are `Cargo.toml`, `.cargo/config.toml`, `rust-toolchain.toml`, `linker.ld`, `.gitignore`, and `AGENTS.md`.
- Add zero external crates to `Cargo.toml` unless the user explicitly authorizes them. Write drivers, allocators, and synchronization primitives from scratch.
- C code, C headers, and assembly-generating C build scripts are forbidden.

## Git Commit Protocol (MANDATORY)

- Give every created or modified file its own dedicated, atomic Git commit immediately after editing and completing any required verification.
- Never bundle multiple files into a single commit.
- Use Conventional Commits format: `<type>(<scope>): <concise description>`. Examples: `feat(uart): implement PL011 serial driver` and `build(linker): define QEMU virt memory layout`.

## Hardware Memory Map Reference (QEMU `virt`)

| Region | Address |
| --- | --- |
| RAM base (Device Tree Blob placement) | `0x40000000` |
| Kernel load address | `0x40080000` |
| PL011 UART0 base | `0x09000000` |
| GICv2 distributor base | `0x08000000` |
| GICv2 CPU interface base | `0x08010000` |

## Verification Workflow

- Always verify compilation with `cargo build` or `cargo check` before committing Rust or assembly changes.
