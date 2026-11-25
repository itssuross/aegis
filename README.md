# Aegis v0.1.0

## Overview & Design Constraints

Aegis is a bare-metal ARM64 hobbyist kernel targeting `aarch64-unknown-none` on QEMU's `virt` machine. It runs at EL1 on a single core and exposes an interactive shell over the PL011 serial console.

- Kernel source is entirely freestanding Rust (`no_std`, `no_main`) and AArch64 assembly. It uses Rust's `core` and `alloc` libraries and has zero external crate dependencies.
- Repository history uses one atomic Conventional Commit per file change.

## Architecture & Subsystems

| File | Responsibility |
| --- | --- |
| [`src/boot.S`](src/boot.S) | Parks secondary cores, enters EL1 if booted at EL2, enables FP/SIMD, sets up the boot stack, and clears `.bss` before calling Rust. |
| [`src/main.rs`](src/main.rs) | Initializes kernel subsystems, checks heap and exception handling at boot, and starts the shell and two demo workers. |
| [`src/mmu.rs`](src/mmu.rs) | Sets up the EL1 identity map for RAM and device MMIO and enables instruction and data caches. |
| [`src/allocator.rs`](src/allocator.rs) | Implements the global heap allocator with an address-ordered free list, block coalescing, and allocation statistics. |
| [`src/exceptions.S`](src/exceptions.S) | Defines the EL1 exception vectors and saves and restores exception frames. |
| [`src/exceptions.rs`](src/exceptions.rs) | Installs the vector table, handles `BRK` traps, and dispatches timer and UART interrupts. |
| [`src/gic.rs`](src/gic.rs) | Configures the GICv2 distributor and CPU interface and acknowledges and completes interrupts. |
| [`src/timer.rs`](src/timer.rs) | Programs the EL1 physical timer for 1 Hz ticks and tracks uptime. |
| [`src/scheduler.rs`](src/scheduler.rs) | Manages kernel threads and switches between them round-robin on timer ticks and cooperative yields. |
| [`src/context.S`](src/context.S) | Saves and restores thread contexts and enters new threads through a trampoline. |
| [`src/uart.rs`](src/uart.rs) | Drives PL011 serial output and drains received bytes into a ring buffer from the UART interrupt handler. |
| [`src/shell.rs`](src/shell.rs) | Consumes buffered UART input, edits command lines, and dispatches shell commands. |
| [`linker.ld`](linker.ld) | Places the kernel image and reserves the boot stack and heap. |

When an interrupt wakes a thread waiting in `wfe`, the kernel and demo workers yield to the next runnable thread. The shell also yields after `wfe` when its receive buffer is empty. This lets UART input reach the shell between 1 Hz timer ticks.

## QEMU `virt` Memory Map

| Address | Region |
| --- | --- |
| `0x0000_0000` | Base of the device MMIO identity mapping, which extends through `0x3FFF_FFFF`. |
| `0x0800_0000` | GICv2 distributor. The CPU interface is at `0x0801_0000`. |
| `0x0900_0000` | PL011 UART0. |
| `0x4000_0000` | RAM base and device tree blob placement. |
| `0x4008_0000` | Kernel image load address. |
| `0x400A_6000` and above | 16 MiB heap in the debug build. The linker places it on the first page boundary after the boot stack, so the base can vary by build. The release build starts at `0x4009_E000`. |

Run `mem` in the shell to see the heap bounds for the running image.

## Prerequisites & Quickstart

Install Rustup and QEMU. The repository selects the nightly toolchain in `rust-toolchain.toml` and the bare-metal target in `.cargo/config.toml`. From the repository root on macOS with Homebrew, run the following commands. [Rustup](https://rustup.rs/)

```sh
rustup target add aarch64-unknown-none
brew install qemu
cargo run
```

`cargo run` boots the debug image on QEMU `virt` with a Cortex-A72 CPU and 128 MiB of RAM. The `aegis>` prompt appears in the current terminal. Enter `halt` to power off the guest.

Build an optimized image with `cargo build --release`. The ELF binary is written to `target/aarch64-unknown-none/release/aegis`.

## Interactive Shell Commands

| Command | Effect |
| --- | --- |
| `help` | Lists available commands. |
| `info` | Shows the kernel version, exception level, CPU ID (`MIDR_EL1`), MMU status, and timer counter frequency. |
| `uptime` | Shows elapsed whole seconds from the 1 Hz timer. |
| `mem` | Shows heap bounds and allocated and free byte counts. |
| `ps` | Lists kernel thread IDs, states, and names. |
| `trace <on\|off>` | Enables or disables timer and scheduler logging. Run `trace` to show the current settings. |
| `brk` | Executes `BRK #0` to demonstrate synchronous exception handling and returns to the prompt. |
| `clear` | Clears the terminal and moves the cursor home using ANSI escape sequences. |
| `reboot` | Restarts the guest via PSCI `SYSTEM_RESET`. |
| `halt` | Powers off the guest via PSCI `SYSTEM_OFF`. |
