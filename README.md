# Aegis v0.6.0

## Overview & Design Constraints

Aegis is a bare-metal ARM64 hobbyist kernel targeting `aarch64-unknown-none` on QEMU's `virt` machine. It runs across 4 Cortex-A72 SMP cores at `EL1`, exposes an interactive shell over the PL011 serial console, and executes concurrent, isolated `EL0` ELF64 user programs with per-process 4 KiB virtual address spaces.

- **v0.6.0** links the kernel at `0xFFFF_FF80_4008_0000` in `TTBR1_EL1` and maps RAM and MMIO linearly with `PHYS_OFFSET = 0xFFFF_FF80_0000_0000`. Per-process `TTBR0_EL1` tables carry only lower-half `EL0` mappings with 16-bit ASIDs, saving the L2 frame previously needed for the kernel RAM identity map. Syscall buffer validation bounds-checks the start and end before probing each page with `AT S1E0R` or `AT S1E0W`.
- The kernel uses `phys_to_virt` to access PL011, GICv2, and VirtIO MMIO registers and dereference PMM frames. VirtIO-Blk queue and buffer DMA addresses and the PSCI `CPU_ON` entry point use physical addresses produced by `virt_to_phys`.
- Per-process file descriptors support console I/O, read-only files, and SMP-safe bounded IPC pipes. The interactive `EL0` shell builds from `user/sh.rs` into `sh.elf` and runs from `disk.img`.
- **Core subsystems:** 4-core SMP bring-up via PSCI `CPU_ON` over `hvc #0`, `TPIDR_EL1`-backed `CpuLocal` per-core state, banked GICv2 CPU interfaces with per-core 100 Hz physical timers, 4-core preemptive round-robin scheduling with per-task `TTBR0_EL1` switching, `IrqSpinLock` and `WaitQueue` synchronization, SMP-serialized PL011 serial I/O, VirtIO MMIO v2 `virtio-blk` driver, and read-only POSIX `ustar` filesystem.
- Kernel source is entirely freestanding Rust (`no_std`, `no_main`) and AArch64 assembly. It uses Rust's `core` and `alloc` libraries and has zero external crate dependencies.
- Repository history uses one atomic Conventional Commit per file change.

## Architecture & Subsystems

| File | Responsibility |
| --- | --- |
| [`src/boot.S`](src/boot.S) | Brings the primary core to EL1, clears `.bss`, and provides the physical `secondary_entry` trampoline for PSCI-started cores. Both paths enable the MMU, move their stacks and `VBAR_EL1` into the higher half, and branch to higher-half Rust entry points. |
| [`src/main.rs`](src/main.rs) | Initializes kernel subsystems on Core 0, brings up secondary cores, initializes the 4 KiB PMM and verifies the 3-level page table lifecycle, runs `secondary_main` on cores 1–3, probes `virtio-blk`, checks heap and exception handling at boot, and starts the shell and demo workers. |
| [`src/platform.rs`](src/platform.rs) | Defines physical RAM and MMIO addresses, `PAGE_SIZE`, `PHYS_OFFSET`, and the `phys_to_virt` and `virt_to_phys` direct-map conversions. |
| [`src/pmm.rs`](src/pmm.rs) | Tracks 4 KiB physical frames in a locked bitmap from the physical address of `__heap_end` to `0x4800_0000`. Returns physical frame addresses and zeroes frames through their higher-half aliases. |
| [`src/smp.rs`](src/smp.rs) | Brings up secondary cores via PSCI `CPU_ON` (`hvc #0`), manages `TPIDR_EL1`-backed `CpuLocal` per-core state and counters, and tracks online CPUs. |
| [`src/mmu.rs`](src/mmu.rs) | Configures 4 KiB granules, 39-bit VA ranges, and 16-bit ASIDs. Owns the shared higher-half `TTBR1_EL1` map and temporary boot identity map, switches kernel threads to an empty `TTBR0_EL1`, and provides three-level user-only page tables and cross-core ASID invalidation. |
| [`src/allocator.rs`](src/allocator.rs) | Implements the global heap allocator with an address-ordered free list, block coalescing, and allocation statistics. |
| [`src/sync.rs`](src/sync.rs) | Provides IRQ-safe `IrqSpinLock` synchronization and interrupt save/restore helpers. |
| [`src/exceptions.S`](src/exceptions.S) | Defines the EL1 and EL0 exception vectors and saves and restores exception frames. |
| [`src/exceptions.rs`](src/exceptions.rs) | Installs the vector table, handles `BRK` traps, dispatches `EL0` `SVC #0` syscalls, validates readable and writable user buffers with hardware `AT S1E0R` and `AT S1E0W`, and dispatches timer and UART interrupts. |
| [`src/gic.rs`](src/gic.rs) | Configures the GICv2 distributor (routing UART0 SPI to Core 0), initializes banked per-core CPU interfaces and timer PPIs, and acknowledges and completes interrupts. |
| [`src/timer.rs`](src/timer.rs) | Programs each core's EL1 physical timer for 100 Hz ticks, updates per-core tick and idle counters, and tracks global millisecond uptime on Core 0. |
| [`src/scheduler.rs`](src/scheduler.rs) | Manages kernel and `EL0` threads across all 4 SMP cores with per-core idle threads, per-task `TTBR0_EL1` switching, `spawn_with_ttbr0`, `join`, sleep timers, `WaitQueue` blocking, and IRQ-safe preemptive round-robin scheduling. |
| [`src/context.S`](src/context.S) | Saves and restores thread contexts and enters new threads through a trampoline. |
| [`src/uart.rs`](src/uart.rs) | Drives PL011 serial output with `IrqSpinLock`-serialized formatted console printing across SMP cores and drains received bytes into a ring buffer, waking waiters from the UART interrupt handler. |
| [`src/virtio.rs`](src/virtio.rs) | Discovers VirtIO MMIO v2 devices and implements split virtqueue descriptor management. |
| [`src/blk.rs`](src/blk.rs) | Reads 512-byte sectors through VirtIO MMIO v2. Accesses device registers through the direct map and submits physical DMA addresses for the request header, data, and status. |
| [`src/fs.rs`](src/fs.rs) | Parses a read-only POSIX `ustar` archive stored on the `virtio-blk` disk. |
| [`src/fd.rs`](src/fd.rs) | Manages per-process descriptor tables for console standard streams, read-only `ustar` files with offsets, and pipe endpoints. |
| [`src/pipe.rs`](src/pipe.rs) | Implements SMP-safe IPC with bounded 1024-byte ring buffers, blocking reads and writes, EOF after the last writer closes, and `EPIPE` after the last reader closes. |
| [`src/user.rs`](src/user.rs) | Copies `ELF64` segments through the higher-half frame aliases and maps only the image and stack at `0x0040_0000..0x0080_0000` in each process's `TTBR0_EL1`. Uses 16-bit ASIDs, `W^X` page flags, and an unmapped `0x007F_E000` stack guard. Manages process I/O, child spawn and wait, and resource reaping. |
| [`src/shell.rs`](src/shell.rs) | Consumes buffered UART input, edits command lines, and dispatches interactive shell commands including `meminfo`, foreground/background `exec <file> [&]`, and `exec-stress [N]`. |
| [`user/hello.rs`](user/hello.rs) | Freestanding `EL0` userspace demo program linked at `0x0040_0000` exercising `SYS_WRITE`, `SYS_SLEEP_MS`, `SYS_SCHED_YIELD`, and `SYS_EXIT`. |
| [`user/sh.rs`](user/sh.rs) | Freestanding interactive `EL0` shell linked as `sh.elf`. Provides line editing, file and console I/O, uptime, child execution, and a pipe round-trip test through syscalls. |
| [`linker.ld`](linker.ld) | Links the kernel at `0xFFFF_FF80_4008_0000`, derives physical load addresses and a physical boot entry at `0x4008_0000`, and reserves space for the boot stack and 16 MiB heap. |

## QEMU `virt` Memory Map

`TTBR0_EL1` covers the 39-bit lower half and uses the following user virtual layout.

| Address | Region |
| --- | --- |
| `0x0040_0000..0x007F_DFFF` | Per-process `EL0` ELF64 virtual address space (3-level 4 KiB `L3` pages, `W^X`, non-global 16-bit ASID). |
| `0x007F_E000..0x007F_EFFF` | Unmapped 4 KiB `EL0` stack guard page. |
| `0x007F_F000..0x007F_FFFF` | Per-process 4 KiB `EL0` user stack (`UserRw`, `UXN=1, PXN=1`, stack top `0x0080_0000`). |

`TTBR1_EL1` provides EL1 access to the following physical regions through their higher-half aliases.

| Region | Physical address | Higher-half alias |
| --- | --- | --- |
| GICv2 distributor and CPU interface | `0x0800_0000`, `0x0801_0000` | `0xFFFF_FF80_0800_0000`, `0xFFFF_FF80_0801_0000` |
| PL011 UART0 | `0x0900_0000` | `0xFFFF_FF80_0900_0000` |
| VirtIO MMIO v2 bus (32 slots, 512 bytes each) | `0x0A00_0000..0x0A00_4000` | `0xFFFF_FF80_0A00_0000..0xFFFF_FF80_0A00_4000` |
| RAM (128 MiB, including the DTB, boot stacks, heap, and PMM pool) | `0x4000_0000..0x4800_0000` | `0xFFFF_FF80_4000_0000..0xFFFF_FF80_4800_0000` |
| Kernel image start | `0x4008_0000` | `0xFFFF_FF80_4008_0000` |

Run `mem` or `meminfo` in the shell to inspect live heap bounds and 4 KiB physical frame pool statistics.

## `EL0` Syscall ABI (`SVC #0`)

The AArch64 syscall convention uses `x8` for the syscall number, `x0`–`x2` for arguments, and `x0` for the return value or negative errno such as `-9` `EBADF`, `-14` `EFAULT`, or `-38` `ENOSYS`. The kernel returns `-EFAULT` if the buffer pointer or its saturated exclusive end reaches `0x0000_0080_0000_0000`. This rejects higher-half kernel addresses and arithmetic overflow before any translation probe. Under the active `TTBR0_EL1`, the kernel validates each spanned 4 KiB page with hardware `AT S1E0R` before reading it and `AT S1E0W` before writing it. Each process starts with `fd 0` for console input and `fd 1` and `fd 2` for console output.

| Number (`x8`) | Name | Arguments | Behavior |
| --- | --- | --- | --- |
| `57` | `SYS_CLOSE` | `x0=fd` | Closes a descriptor. Returns `0` or negative errno. |
| `63` | `SYS_READ` | `x0=fd`, `x1=ptr`, `x2=len` | Reads up to one page into the user buffer from console input, a file, or a pipe. Returns bytes read, `0` at EOF, or negative errno. |
| `64` | `SYS_WRITE` | `x0=fd`, `x1=ptr`, `x2=len` | Writes to console output or a pipe. Returns bytes written or negative errno. |
| `93` | `SYS_EXIT` | `x0=code` | Switches back to the empty `TTBR0_EL1`, closes descriptors, releases address-space frames and ASID, and terminates the process with a signed 32-bit status. |
| `124` | `SYS_SCHED_YIELD` | — | Yields the CPU to the next runnable thread. Returns `0`. |
| `200` | `SYS_SLEEP_MS` | `x0=ms` | Sleeps the calling thread for `ms` milliseconds on the 100 Hz timer. Returns `0`. |
| `201` | `SYS_OPEN` | `x0=path_ptr`, `x1=path_len` | Opens a read-only `ustar` file by its UTF-8 path. Returns a descriptor or negative errno. |
| `202` | `SYS_PIPE` | `x0=pair_ptr` | Writes the read and write descriptors, in that order, to a user array of two `u32` values. Returns `0` or negative errno. |
| `203` | `SYS_SPAWN` | `x0=path_ptr`, `x1=path_len` | Starts a child `EL0` ELF from a UTF-8 path. Returns its PID or negative errno. |
| `204` | `SYS_WAITPID` | `x0=pid` | Waits for the specified child process. Returns its signed exit status or negative errno. |
| `205` | `SYS_UPTIME_MS` | — | Returns elapsed uptime in milliseconds. |

## Prerequisites & Quickstart

Install Rustup and QEMU. The repository selects the nightly toolchain in `rust-toolchain.toml` and the bare-metal target in `.cargo/config.toml`. From the repository root on macOS with Homebrew, run the following commands. [Rustup](https://rustup.rs/)

```sh
rustup target add aarch64-unknown-none
brew install qemu
cargo run
```

`cargo run` automatically compiles `hello.elf` and `sh.elf` at `0x0040_0000`, packs them with `motd.txt` and `readme.txt` into a POSIX `ustar` `disk.img` when missing or stale, and boots the debug image on QEMU `virt` with 4 Cortex-A72 CPUs (`-smp 4`), 128 MiB of RAM, and a modern `virtio-blk` drive. At `aegis>`, enter `exec sh.elf` to open the `sh>` userspace prompt. Use `exit` to return to `aegis>` and `halt` to power off the guest.

Build an optimized image with `cargo build --release`. The ELF binary is written to `target/aarch64-unknown-none/release/aegis`.

Run the automated QEMU regression suite to verify debug and release builds, v0.6.0 higher-half telemetry, 4-core SMP bring-up, the 3-level page table lifecycle, concurrent `exec-stress 4`, missing-file recovery, and the interactive `EL0` shell. The suite checks file I/O, child execution, pipe data and EOF, and zero used PMM frames immediately before halt.

```sh
./scripts/test-qemu.sh
```

## Interactive Shell Commands

| Command | Effect |
| --- | --- |
| `help` | Lists available commands. |
| `info` | Shows the kernel version, exception level, CPU ID (`MIDR_EL1`), MMU status, `TTBR1_EL1` kernel VMA, and timer counter frequency. |
| `cpuinfo` | Shows online CPU count and per-core state, timer ticks, idle ticks, and context switch counters. |
| `uptime` | Shows elapsed uptime from the 100 Hz timer. |
| `mem` | Shows heap bounds and allocated and free byte counts. |
| `meminfo` | Shows 4 KiB PMM frame pool bounds, total/used/free frames, and kernel heap statistics. |
| `ps` | Lists kernel thread IDs, states, assigned `CPU` (`-` when not running), and names. |
| `blkinfo` | Shows the attached `virtio-blk` slot, MMIO base address, sector count, and capacity. |
| `ls` | Lists files and directories in the `ustar` filesystem on `disk.img`. |
| `cat <file>` | Prints the contents of a file from the `ustar` filesystem. |
| `exec <file> [&]` | Loads and runs an `EL0` ELF64 binary from the `ustar` filesystem. Append `&` to spawn in the background and return immediately with its PID. |
| `exec sh.elf` | Opens the interactive `EL0` shell at `sh>`. |
| `exec-stress [N]` | Spawns `N` (default `4`, clamped to `1..=8`) concurrent `hello.elf` `EL0` processes across all 4 SMP cores, awaits all via `wait_pid`, and reports completion counts. |
| `trace <on\|off>` | Enables or disables timer and scheduler logging. Run `trace` to show the current settings. |
| `brk` | Executes `BRK #0` to demonstrate synchronous exception handling and returns to the prompt. |
| `clear` | Clears the terminal and moves the cursor home using ANSI escape sequences. |
| `reboot` | Restarts the guest via PSCI `SYSTEM_RESET`. |
| `halt` | Powers off the guest via PSCI `SYSTEM_OFF`. |

At `sh>`, `help` lists `echo <text>`, `cat <file>`, `uptime`, `run <file>`, `pipetest`, and `exit [code]`. Try `run hello.elf` to start a child process and `pipetest` to verify a pipe round trip and EOF. An omitted exit code defaults to `0`.
