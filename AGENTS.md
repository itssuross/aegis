# Aegis Engineering Instructions (v0.6.0+)

- **Name:** Aegis (`itssuross/aegis`)
- **Architecture:** Bare-metal ARM64 (`aarch64-unknown-none`) multi-core hobbyist kernel
- **Target Hardware:** QEMU `virt` (`-M virt -cpu cortex-a72 -m 128M -smp 4 -nographic -serial mon:stdio` + `virtio-blk-device`)

## Language, Edition & Dependency Rules (STRICT)

- Use strictly Rust 2024 (`#![no_std]`, `#![no_main]`) and AArch64 assembly (`.S`) for all kernel and `EL0` userspace source code.
- Permitted non-source files: `Cargo.toml`, `Cargo.lock`, `.cargo/config.toml`, `rust-toolchain.toml`, `linker.ld`, `scripts/*.sh`, `README.md`, `.gitignore`, and `AGENTS.md`.
- **Zero External Crates:** Never add dependencies to `Cargo.toml`. Implement all drivers, allocators, page tables, IPC primitives, and ELF loaders from scratch.
- **Zero C:** C source files, C headers, and C-generating build scripts are strictly forbidden.
- Use `#[unsafe(no_mangle)]` for unmangled symbols and `unsafe extern "C"` for foreign declarations. Put unsafe operations in explicit `unsafe` blocks, including inside `unsafe fn`. Form pointers to mutable statics with `&raw const` or `&raw mut` and avoid references to `static mut`.

## Architecture & Memory Invariants (`v0.6.0+`)

| Region / Constant | Address / Value | Notes |
| --- | --- | --- |
| `PHYS_OFFSET` | `0xFFFF_FF80_0000_0000` | Linear higher-half kernel offset (`TTBR1_EL1`) |
| Physical RAM | `0x4000_0000..0x4800_0000` | 128 MiB QEMU `virt` RAM (`0x4008_0000` physical load) |
| Higher-Half Kernel VMA | `0xFFFF_FF80_4008_0000+` | Linked VMA (`TTBR1_EL1`, 39-bit VA, `T1SZ=25`, `TG1=0b10`) |
| GICv2 Distributor / CPU | `0x0800_0000` / `0x0801_0000` | Access via `phys_to_virt(...)` |
| PL011 UART0 | `0x0900_0000` | Access via `phys_to_virt(...)` |
| VirtIO-MMIO Bus | `0x0A00_0000` (stride `0x200`) | Access MMIO via `phys_to_virt(...)`; program DMA rings via `virt_to_phys(...)` |
| `EL0` User Address Space | `0x0040_0000..0x0080_0000` | Pure lower-half `TTBR0_EL1` with 16-bit ASIDs and an unmapped guard page at `0x007F_E000` |

- Drop the temporary boot identity map after SMP bring-up. Keep kernel RAM, code, stacks, vectors, and MMIO in `TTBR1_EL1`. Use an empty `TTBR0_EL1` for kernel threads and map only `EL0` pages in process tables.
- Keep PMM frame addresses and page-table descriptors physical. Use `crate::platform::phys_to_virt(phys)` before dereferencing a frame, accessing RAM through the direct map, or accessing MMIO. Use `crate::platform::virt_to_phys(virt)` only on direct-map addresses when programming DMA rings and buffers or passing the PSCI `CPU_ON` entry point (`hvc #0`).
- Reject syscall buffers with either `ptr` or `ptr.saturating_add(len)` at or above `0x0000_0080_0000_0000` with `-EFAULT` (`-14`). For nonempty buffers, validate every spanned 4 KiB page with `AT S1E0R` for reads or `AT S1E0W` for writes. Mask IRQs across the probes and test `PAR_EL1 & 1` for faults.
- Acquire `SCHEDULER` before `WaitQueue` locks when both are needed. Keep `SCHEDULER` held across `switch_context` and preserve the scheduler's lock handoff and IRQ state.

## Comment Hygiene (STRICT)

- Ban narrative comments, line-by-line commentary, section dividers, and redundant doc-block bloat.
- Allow comments ONLY for:
  - `// SAFETY:` invariants on `unsafe` blocks and functions.
  - Brief inline hardware register, bitfield, or ABI notes not obvious from symbol names.

## Git Commit Protocol (MANDATORY)

- Commit every created or modified file in its own dedicated, atomic Git commit immediately after editing and verifying it.
- Stage only that file and verify the commit touches one path. Commit, push, tag, or publish only with user authorization.
- Use Conventional Commits: `<type>(<scope>): <concise description>` (e.g., `feat(mmu): ...`, `fix(exceptions): ...`, `test(scripts): ...`, `docs(readme): ...`).

## Verification & Release Workflow

- Run `cargo build` or `cargo check` before committing Rust or assembly changes. Run `cargo build` and `cargo build --release` with zero warnings before a release.
- Run `./scripts/test-qemu.sh` before finalizing a milestone. It verifies 4-core SMP, higher-half telemetry, VirtIO-Blk, `EL0` execution, interactive file, spawn, and pipe operations, and zero used PMM frames before halt.
