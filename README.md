# Aegis v0.4.0

## Overview & Design Constraints

Aegis is a bare-metal ARM64 hobbyist kernel targeting `aarch64-unknown-none` on QEMU's `virt` machine. It runs across 4 Cortex-A72 SMP cores at `EL1`, exposes an interactive shell over the PL011 serial console, and executes concurrent, isolated `EL0` ELF64 user programs with per-process 4 KiB virtual address spaces.

- **v0.4.0 highlights:** Centralized hardware memory map (`src/platform.rs`), SMP-safe 4 KiB physical frame bitmap allocator (`src/pmm.rs`), 3-level 4 KiB paging (`L1 -> L2 -> L3`) with 16-bit hardware ASIDs (`TTBR0_EL1[63:48]`), per-process `EL0` ELF64 loading at `0x0040_0000` with strict `W^X` segment permissions and unmapped `0x007F_E000` stack guard pages, hardware `AT S1E0R` syscall buffer validation, concurrent multi-core `EL0` execution with `wait_pid` and automatic resource reaping, and an automated QEMU SMP/PMM/EL0 regression harness (`./scripts/test-qemu.sh`).
- **Core subsystems:** 4-core SMP bring-up via PSCI `CPU_ON` over `hvc #0`, `TPIDR_EL1`-backed `CpuLocal` per-core state, banked GICv2 CPU interfaces with per-core 100 Hz physical timers, 4-core preemptive round-robin scheduling with per-task `TTBR0_EL1` switching, `IrqSpinLock` and `WaitQueue` synchronization, SMP-serialized PL011 serial I/O, VirtIO MMIO v2 `virtio-blk` driver, and read-only POSIX `ustar` filesystem.
- Kernel source is entirely freestanding Rust (`no_std`, `no_main`) and AArch64 assembly. It uses Rust's `core` and `alloc` libraries and has zero external crate dependencies.
- Repository history uses one atomic Conventional Commit per file change.

## Architecture & Subsystems

| File | Responsibility |
| --- | --- |
| [`src/boot.S`](src/boot.S) | Parks secondary cores on initial boot, enters EL1 if booted at EL2, enables FP/SIMD, sets up Core 0's boot stack and clears `.bss`, and provides `secondary_entry` with per-core 16 KiB stacks and `VBAR_EL1` setup for PSCI-started cores. |
| [`src/main.rs`](src/main.rs) | Initializes kernel subsystems on Core 0, brings up secondary cores, initializes the 4 KiB PMM and verifies the 3-level page table lifecycle, runs `secondary_main` on cores 1–3, probes `virtio-blk`, checks heap and exception handling at boot, and starts the shell and demo workers. |
| [`src/platform.rs`](src/platform.rs) | Centralizes QEMU `virt` hardware constants (`PAGE_SIZE`, `RAM_BASE`/`RAM_END`, GICv2, PL011 UART0, and VirtIO MMIO addresses). |
| [`src/pmm.rs`](src/pmm.rs) | Implements an `IrqSpinLock`-protected 4 KiB physical frame bitmap allocator managing all RAM above `__heap_end` through `0x4800_0000`, with frame zeroing and live usage statistics. |
| [`src/smp.rs`](src/smp.rs) | Brings up secondary cores via PSCI `CPU_ON` (`hvc #0`), manages `TPIDR_EL1`-backed `CpuLocal` per-core state and counters, and tracks online CPUs. |
| [`src/mmu.rs`](src/mmu.rs) | Configures `MAIR_EL1`/`TCR_EL1` (4 KiB granule, 39-bit VA, 16-bit ASIDs in `TTBR0_EL1[63:48]`), owns the boot kernel `L1_TABLE`, exposes `PageTable` for 3-level (`L1 -> L2 -> L3`) 4 KiB user mappings and `L2` kernel mappings, and provides `switch_ttbr0`, `flush_asid`, and `init_secondary()`. |
| [`src/allocator.rs`](src/allocator.rs) | Implements the global heap allocator with an address-ordered free list, block coalescing, and allocation statistics. |
| [`src/sync.rs`](src/sync.rs) | Provides IRQ-safe `IrqSpinLock` synchronization and interrupt save/restore helpers. |
| [`src/exceptions.S`](src/exceptions.S) | Defines the EL1 and EL0 exception vectors and saves and restores exception frames. |
| [`src/exceptions.rs`](src/exceptions.rs) | Installs the vector table, handles `BRK` traps and `EL0` `SVC #0` syscalls, validates user pointers via hardware `AT S1E0R` translation, and dispatches timer and UART interrupts. |
| [`src/gic.rs`](src/gic.rs) | Configures the GICv2 distributor (routing UART0 SPI to Core 0), initializes banked per-core CPU interfaces and timer PPIs, and acknowledges and completes interrupts. |
| [`src/timer.rs`](src/timer.rs) | Programs each core's EL1 physical timer for 100 Hz ticks, updates per-core tick and idle counters, and tracks global millisecond uptime on Core 0. |
| [`src/scheduler.rs`](src/scheduler.rs) | Manages kernel and `EL0` threads across all 4 SMP cores with per-core idle threads, per-task `TTBR0_EL1` switching, `spawn_with_ttbr0`, `join`, sleep timers, `WaitQueue` blocking, and IRQ-safe preemptive round-robin scheduling. |
| [`src/context.S`](src/context.S) | Saves and restores thread contexts and enters new threads through a trampoline. |
| [`src/uart.rs`](src/uart.rs) | Drives PL011 serial output with `IrqSpinLock`-serialized formatted console printing across SMP cores and drains received bytes into a ring buffer, waking waiters from the UART interrupt handler. |
| [`src/virtio.rs`](src/virtio.rs) | Discovers VirtIO MMIO v2 devices and implements split virtqueue descriptor management. |
| [`src/blk.rs`](src/blk.rs) | Implements the VirtIO MMIO v2 `virtio-blk` block device driver for 512-byte sector reads. |
| [`src/fs.rs`](src/fs.rs) | Parses a read-only POSIX `ustar` archive stored on the `virtio-blk` disk. |
| [`src/user.rs`](src/user.rs) | Loads `ELF64` binaries into per-process 4 KiB `TTBR0_EL1` address spaces (`0x0040_0000`) with 16-bit ASIDs, strict `W^X` page flags, and unmapped `0x007F_E000` stack guard pages; supports concurrent multi-core `EL0` execution via `exec_elf`, `spawn_elf`, `wait_pid`, and automatic background resource reaping. |
| [`src/shell.rs`](src/shell.rs) | Consumes buffered UART input, edits command lines, and dispatches interactive shell commands including `meminfo`, foreground/background `exec <file> [&]`, and `exec-stress [N]`. |
| [`user/hello.rs`](user/hello.rs) | Freestanding `EL0` userspace demo program linked at `0x0040_0000` exercising `SYS_WRITE`, `SYS_SLEEP_MS`, `SYS_SCHED_YIELD`, and `SYS_EXIT`. |
| [`linker.ld`](linker.ld) | Places the kernel image and reserves the boot stack and heap. |

## QEMU `virt` Memory Map

| Address | Region |
| --- | --- |
| `0x0040_0000..0x007F_DFFF` | Per-process `EL0` ELF64 virtual address space (3-level 4 KiB `L3` pages, `W^X`, non-global 16-bit ASID). |
| `0x007F_E000..0x007F_EFFF` | Unmapped 4 KiB `EL0` stack guard page. |
| `0x007F_F000..0x007F_FFFF` | Per-process 4 KiB `EL0` user stack (`UserRw`, `UXN=1, PXN=1`, stack top `0x0080_0000`). |
| `0x0800_0000` | GICv2 distributor (`Device-nGnRnE`, `EL1`-only). CPU interface is at `0x0801_0000`. |
| `0x0900_0000` | PL011 UART0 (`Device-nGnRnE`, `EL1`-only). |
| `0x0a00_0000..0x0a00_3e00` | VirtIO MMIO v2 bus (32 slots, 512 bytes each). |
| `0x4000_0000..0x47FF_FFFF` | 128 MiB physical RAM (`EL1`-only RW, `UXN=1`): DTB at `0x4000_0000`, kernel image at `0x4008_0000`, boot stack, 16 MiB heap, and PMM-managed 4 KiB physical frame pool through `0x4800_0000`. |

Run `mem` or `meminfo` in the shell to inspect live heap bounds and 4 KiB physical frame pool statistics.

## `EL0` Syscall ABI (`SVC #0`)

Syscalls follow the AArch64 convention: `x8` holds the syscall number, `x0`–`x2` hold arguments, and `x0` receives the return value (or negative errno such as `-9` `EBADF`, `-14` `EFAULT`, `-38` `ENOSYS`). User buffer pointers are validated per 4 KiB page using hardware `AT S1E0R` translation under the active `TTBR0_EL1`.

| Number (`x8`) | Name | Arguments | Behavior |
| --- | --- | --- | --- |
| `64` | `SYS_WRITE` | `x0=fd`, `x1=ptr`, `x2=len` | Validates `ptr..ptr+len` with `AT S1E0R` and writes `len` bytes to PL011 UART (`fd` must be `1` or `2`). Returns bytes written or negative errno. |
| `93` | `SYS_EXIT` | `x0=code` | Switches back to kernel `TTBR0_EL1`, releases address-space frames/ASID, and terminates the `EL0` process with exit status `code`. |
| `124` | `SYS_SCHED_YIELD` | — | Yields the CPU to the next runnable thread. Returns `0`. |
| `200` | `SYS_SLEEP_MS` | `x0=ms` | Sleeps the calling thread for `ms` milliseconds on the 100 Hz timer. Returns `0`. |

## Prerequisites & Quickstart

Install Rustup and QEMU. The repository selects the nightly toolchain in `rust-toolchain.toml` and the bare-metal target in `.cargo/config.toml`. From the repository root on macOS with Homebrew, run the following commands. [Rustup](https://rustup.rs/)

```sh
rustup target add aarch64-unknown-none
brew install qemu
cargo run
```

`cargo run` automatically compiles `user/hello.rs` into `hello.elf` (linked at `0x0040_0000`), packs a 64 KiB POSIX `ustar` `disk.img` (containing `motd.txt`, `readme.txt`, and `hello.elf`) when missing or stale, and boots the debug image on QEMU `virt` with 4 Cortex-A72 CPUs (`-smp 4`), 128 MiB of RAM, and a modern `virtio-blk` drive. The `aegis>` prompt appears in the current terminal. Enter `halt` to power off the guest.

Build an optimized image with `cargo build --release`. The ELF binary is written to `target/aarch64-unknown-none/release/aegis`.

Run the automated QEMU regression suite to verify debug/release builds, 4-core SMP bring-up, 3-level page table lifecycle, concurrent 4-core `exec-stress 4`, zero leaked PMM frames, and graceful missing-file recovery:

```sh
./scripts/test-qemu.sh
```

## Interactive Shell Commands

| Command | Effect |
| --- | --- |
| `help` | Lists available commands. |
| `info` | Shows the kernel version, exception level, CPU ID (`MIDR_EL1`), MMU status, and timer counter frequency. |
| `cpuinfo` | Shows online CPU count and per-core state, timer ticks, idle ticks, and context switch counters. |
| `uptime` | Shows elapsed uptime from the 100 Hz timer. |
| `mem` | Shows heap bounds and allocated and free byte counts. |
| `meminfo` | Shows 4 KiB PMM frame pool bounds, total/used/free frames, and kernel heap statistics. |
| `ps` | Lists kernel thread IDs, states, assigned `CPU` (`-` when not running), and names. |
| `blkinfo` | Shows the attached `virtio-blk` slot, MMIO base address, sector count, and capacity. |
| `ls` | Lists files and directories in the `ustar` filesystem on `disk.img`. |
| `cat <file>` | Prints the contents of a file from the `ustar` filesystem. |
| `exec <file> [&]` | Loads and runs an `EL0` ELF64 binary from the `ustar` filesystem; append `&` to spawn in the background and return immediately with its PID. |
| `exec-stress [N]` | Spawns `N` (default `4`, clamped to `1..=8`) concurrent `hello.elf` `EL0` processes across all 4 SMP cores, awaits all via `wait_pid`, and reports completion counts. |
| `trace <on\|off>` | Enables or disables timer and scheduler logging. Run `trace` to show the current settings. |
| `brk` | Executes `BRK #0` to demonstrate synchronous exception handling and returns to the prompt. |
| `clear` | Clears the terminal and moves the cursor home using ANSI escape sequences. |
| `reboot` | Restarts the guest via PSCI `SYSTEM_RESET`. |
| `halt` | Powers off the guest via PSCI `SYSTEM_OFF`. |
