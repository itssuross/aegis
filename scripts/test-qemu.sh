#!/usr/bin/env sh
set -e

cargo build
cargo build --release

tmpdir=$(mktemp -d)
log="$tmpdir/qemu.log"
fifo="$tmpdir/qemu.in"
mkfifo "$fifo"
: > "$log"

qemu_pid=""
feeder_pid=""
watchdog_pid=""

cleanup() {
    if [ -n "$watchdog_pid" ]; then
        pkill -P "$watchdog_pid" 2>/dev/null || true
        kill "$watchdog_pid" 2>/dev/null || true
    fi
    if [ -n "$feeder_pid" ]; then
        pkill -P "$feeder_pid" 2>/dev/null || true
        kill "$feeder_pid" 2>/dev/null || true
    fi
    if [ -n "$qemu_pid" ]; then
        pkill -P "$qemu_pid" 2>/dev/null || true
        kill "$qemu_pid" 2>/dev/null || true
    fi
    rm -f "$log" "$fifo"
    rmdir "$tmpdir" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

cargo run < "$fifo" > "$log" 2>&1 &
qemu_pid=$!

(
    ticks=0
    while [ "$ticks" -lt 150 ]; do
        if grep -Fq "aegis> " "$log" 2>/dev/null; then
            break
        fi
        sleep 0.1
        ticks=$((ticks + 1))
    done

    for cmd in \
        "info" \
        "meminfo" \
        "cpuinfo" \
        "ps" \
        "blkinfo" \
        "ls" \
        "cat motd.txt" \
        "exec hello.elf" \
        "halt"
    do
        printf '%s\n' "$cmd"
        sleep 0.15
    done
) > "$fifo" 2>/dev/null &
feeder_pid=$!

(
    sleep 25
    pkill -P "$qemu_pid" 2>/dev/null || true
    kill -TERM "$qemu_pid" 2>/dev/null || true
    sleep 1
    pkill -KILL -P "$qemu_pid" 2>/dev/null || true
    kill -KILL "$qemu_pid" 2>/dev/null || true
) >/dev/null 2>&1 &
watchdog_pid=$!

set +e
wait "$qemu_pid"
qemu_status=$?
set -e
qemu_pid=""

pkill -P "$watchdog_pid" 2>/dev/null || true
kill "$watchdog_pid" 2>/dev/null || true
watchdog_pid=""
pkill -P "$feeder_pid" 2>/dev/null || true
kill "$feeder_pid" 2>/dev/null || true
feeder_pid=""

if [ "$qemu_status" -ne 0 ]; then
    echo "FAIL: QEMU exited with status $qemu_status" >&2
    cat "$log" >&2
    exit 1
fi

assert_contains() {
    if ! grep -Fq "$1" "$log"; then
        echo "FAIL: expected output missing: $1" >&2
        cat "$log" >&2
        exit 1
    fi
}

assert_contains "active cores: 4/4"
assert_contains "[pmm]"
assert_contains "0 frames leaked"
assert_contains "managed   :"
assert_contains "online: 4/4"
assert_contains "Welcome to Aegis"
assert_contains "[exec] exited with code 0"
assert_contains "Halting via PSCI..."

echo "PASS: QEMU SMP, PMM, and EL0 regression checks succeeded"
