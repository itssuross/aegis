# Aegis v0.3.0

## Overview & Design Constraints

Aegis is a bare-metal ARM64 hobbyist kernel targeting `aarch64-unknown-none` on QEMU's `virt` machine. It runs across 4 Cortex-A72 SMP cores at `EL1`, exposes an interactive shell over the PL011 serial console, and executes isolated `EL0` ELF64 user programs.

- **v0.3.0 subsystems:** 4-core SMP bring-up via PSCI `CPU_ON` over `hvc #0`, `TPIDR_EL1`-backed `CpuLocal` per-core state, banked GICv2 CPU interfaces with per-core 100 Hz physical timers, 4-core preemptive round-robin scheduling with `IrqSpinLock` and `WaitQueue` blocking, SMP-serialized serial output, VirtIO MMIO v2 `virtio-blk` driver, read-only POSIX `ustar` filesystem, Level 2 2 MiB page table isolation, and `EL0` ELF64 execution with `SVC #0` syscalls.
- Kernel source is entirely freestanding Rust (`no_std`, `no_main`) and AArch64 assembly. It uses Rust's `core` and `alloc` libraries and has zero external crate dependencies.
- Repository history uses one atomic Conventional Commit per file change.

## Architecture & Subsystems

| File | Responsibility |
| --- | --- |
| [`src/boot.S`](src/boot.S) | Parks secondary cores on initial boot, enters EL1 if booted at EL2, enables FP/SIMD, sets up Core 0's boot stack and clears `.bss`, and provides `secondary_entry` with per-core 16 KiB stacks and `VBAR_EL1` setup for PSCI-started cores. |
| [`src/main.rs`](src/main.rs) | Initializes kernel subsystems on Core 0, brings up secondary cores, runs `secondary_main` on cores 1–3, probes `virtio-blk`, checks heap and exception handling at boot, and starts the shell and demo workers. |
| [`src/smp.rs`](src/smp.rs) | Brings up secondary cores via PSCI `CPU_ON` (`hvc #0`), manages `TPIDR_EL1`-backed `CpuLocal` per-core state and counters, and tracks online CPUs. |
| [`src/mmu.rs`](src/mmu.rs) | Builds the shared L1/L2 identity map with 2 MiB RAM blocks, isolates the `EL0` window (`0x4200_0000..0x421F_FFFF`), enables instruction and data caches on Core 0, and exposes `init_secondary()` for secondary cores. |
| [`src/allocator.rs`](src/allocator.rs) | Implements the global heap allocator with an address-ordered free list, block coalescing, and allocation statistics. |
| [`src/sync.rs`](src/sync.rs) | Provides IRQ-safe `IrqSpinLock` synchronization and `WaitQueue` blocking primitives. |
| [`src/exceptions.S`](src/exceptions.S) | Defines the EL1 and EL0 exception vectors and saves and restores exception frames. |
| [`src/exceptions.rs`](src/exceptions.rs) | Installs the vector table, handles `BRK` traps and `EL0` `SVC #0` syscalls, and dispatches timer and UART interrupts. |
| [`src/gic.rs`](src/gic.rs) | Configures the GICv2 distributor (routing UART0 SPI to Core 0), initializes banked per-core CPU interfaces and timer PPIs, and acknowledges and completes interrupts. |
| [`src/timer.rs`](src/timer.rs) | Programs each core's EL1 physical timer for 100 Hz ticks, updates per-core tick and idle counters, and tracks global millisecond uptime on Core 0. |
| [`src/scheduler.rs`](src/scheduler.rs) | Manages kernel threads across all 4 SMP cores with per-core idle threads, CPU pinning, sleep timers, `WaitQueue` blocking, and IRQ-safe preemptive round-robin scheduling. |
| [`src/context.S`](src/context.S) | Saves and restores thread contexts and enters new threads through a trampoline. |
| [`src/uart.rs`](src/uart.rs) | Drives PL011 serial output with `IrqSpinLock`-serialized formatted console printing across SMP cores and drains received bytes into a ring buffer, waking waiters from the UART interrupt handler. |
| [`src/virtio.rs`](src/virtio.rs) | Discovers VirtIO MMIO v2 devices and implements split virtqueue descriptor management. |
| [`src/blk.rs`](src/blk.rs) | Implements the VirtIO MMIO v2 `virtio-blk` block device driver for 512-byte sector reads. |
| [`src/fs.rs`](src/fs.rs) | Parses a read-only POSIX `ustar` archive stored on the `virtio-blk` disk. |
| [`src/user.rs`](src/user.rs) | Loads `ELF64` binaries into the isolated 2 MiB `EL0` region, pins the calling thread to its current core with exclusive `EL0` ownership, enters `EL0` via `ERET`, and returns exit status to `EL1`. |
| [`src/shell.rs`](src/shell.rs) | Consumes buffered UART input, edits command lines, and dispatches interactive shell commands. |
| [`user/hello.rs`](user/hello.rs) | Freestanding `EL0` userspace demo program exercising `SYS_WRITE`, `SYS_SLEEP_MS`, `SYS_SCHED_YIELD`, and `SYS_EXIT`. |
| [`linker.ld`](linker.ld) | Places the kernel image and reserves the boot stack and heap. |

## QEMU `virt` Memory Map

| Address | Region |
| --- | --- |
| `0x0000_0000..0x3FFF_FFFF` | Device MMIO L1 block identity mapping (`Device-nGnRnE`, `PXN=1, UXN=1`). |
| `0x0800_0000` | GICv2 distributor. The CPU interface is at `0x0801_0000`. |
| `0x0900_0000` | PL011 UART0. |
| `0x0a00_0000..0x0a00_3e00` | VirtIO MMIO v2 bus (32 slots, 512 bytes each). |
| `0x4000_0000..0x41FF_FFFF` | Kernel RAM (L2 2 MiB blocks, `EL1`-only RW with `UXN=1`); includes DTB at `0x4000_0000`, kernel image at `0x4008_0000`, boot stack, and 16 MiB heap. |
| `0x4200_0000..0x421F_FFFF` | Isolated 2 MiB `EL0` userspace window (L2 block 16, `AP[1]=1, UXN=0, PXN=1`). |

Run `mem` in the shell to see the heap bounds for the running image.

## `EL0` Syscall ABI (`SVC #0`)

Syscalls follow the AArch64 convention: `x8` holds the syscall number, `x0`–`x2` hold arguments, and `x0` receives the return value (or negative errno such as `-9` `EBADF`, `-14` `EFAULT`, `-38` `ENOSYS`).

| Number (`x8`) | Name | Arguments | Behavior |
| --- | --- | --- | --- |
| `64` | `SYS_WRITE` | `x0=fd`, `x1=ptr`, `x2=len` | Writes `len` bytes from `EL0` RAM to PL011 UART (`fd` must be `1` or `2`). Returns bytes written or negative errno. |
| `93` | `SYS_EXIT` | `x0=code` | Terminates the `EL0` program and returns `code` to `EL1`. |
| `124` | `SYS_SCHED_YIELD` | — | Yields the CPU to the next runnable thread. Returns `0`. |
| `200` | `SYS_SLEEP_MS` | `x0=ms` | Sleeps the calling thread for `ms` milliseconds on the 100 Hz timer. Returns `0`. |

## Prerequisites & Quickstart

Install Rustup and QEMU. The repository selects the nightly toolchain in `rust-toolchain.toml` and the bare-metal target in `.cargo/config.toml`. From the repository root on macOS with Homebrew, run the following commands. [Rustup](https://rustup.rs/)

```sh
rustup target add aarch64-unknown-none
brew install qemu
cargo run
```

`cargo run` automatically compiles `user/hello.rs` into `hello.elf`, packs a 64 KiB POSIX `ustar` `disk.img` (containing `motd.txt`, `readme.txt`, and `hello.elf`) when missing or stale, and boots the debug image on QEMU `virt` with 4 Cortex-A72 CPUs (`-smp 4`), 128 MiB of RAM, and a modern `virtio-blk` drive. The `aegis>` prompt appears in the current terminal. Enter `halt` to power off the guest.

Build an optimized image with `cargo build --release`. The ELF binary is written to `target/aarch64-unknown-none/release/aegis`.

## Interactive Shell Commands

| Command | Effect |
| --- | --- |
| `help` | Lists available commands. |
| `info` | Shows the kernel version, exception level, CPU ID (`MIDR_EL1`), MMU status, and timer counter frequency. |
| `cpuinfo` | Shows online CPU count and per-core state, timer ticks, idle ticks, and context switch counters. |
| `uptime` | Shows elapsed uptime from the 100 Hz timer. |
| `mem` | Shows heap bounds and allocated and free byte counts. |
| `ps` | Lists kernel thread IDs, states, assigned `CPU` (`-` when not running), and names. |
| `blkinfo` | Shows the attached `virtio-blk` slot, MMIO base address, sector count, and capacity. |
| `ls` | Lists files and directories in the `ustar` filesystem on `disk.img`. |
| `cat <file>` | Prints the contents of a file from the `ustar` filesystem. |
| `exec <file>` | Loads and runs an `EL0` ELF64 binary from the `ustar` filesystem and prints its exit code. |
| `trace <on\|off>` | Enables or disables timer and scheduler logging. Run `trace` to show the current settings. |
| `brk` | Executes `BRK #0` to demonstrate synchronous exception handling and returns to the prompt. |
| `clear` | Clears the terminal and moves the cursor home using ANSI escape sequences. |
| `reboot` | Restarts the guest via PSCI `SYSTEM_RESET`. |
| `halt` | Powers off the guest via PSCI `SYSTEM_OFF`. |
