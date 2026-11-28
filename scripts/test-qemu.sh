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
    wait_for_log() {
        ticks=0
        while [ "$ticks" -lt 150 ]; do
            if [ "$(grep -Fc "$1" "$log")" -ge "${2:-1}" ]; then
                return 0
            fi
            sleep 0.1
            ticks=$((ticks + 1))
        done
        return 1
    }

    wait_for_log "aegis> "

    for cmd in \
        "info" \
        "meminfo" \
        "cpuinfo" \
        "ps" \
        "blkinfo" \
        "ls" \
        "cat motd.txt" \
        "exec hello.elf"
    do
        printf '%s\n' "$cmd"
        sleep 0.15
    done

    wait_for_log "[exec] exited with code 0"
    printf 'exec-stress 4\n'
    wait_for_log "[exec-stress] completed 4/4 processes (ok=4, err=0)"

    printf 'meminfo\n'
    wait_for_log "used      : 0 frames" 2
    printf 'exec missing.elf\n'
    wait_for_log "exec: fs: file not found"
    printf 'exec sh.elf\n'

    prompt=1
    for cmd in \
        "help" \
        "echo hello from el0 shell" \
        "cat motd.txt" \
        "uptime" \
        "run hello.elf" \
        "pipetest" \
        "exit"
    do
        wait_for_log "sh> " "$prompt"
        printf '%s\n' "$cmd"
        prompt=$((prompt + 1))
    done

    wait_for_log "[exec] exited with code 0" 2
    printf 'meminfo\n'
    wait_for_log "used      : 0 frames" 3
    printf 'halt\n'
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
assert_contains "Aegis v0.6.0 (aarch64-unknown-none)"
assert_contains "KernelVMA  : 0xFFFF_FF80_4008_0000 (TTBR1_EL1)"
assert_contains "[pmm]"
assert_contains "0 frames leaked"
assert_contains "managed   :"
assert_contains "online: 4/4"
assert_contains "Welcome to Aegis"
assert_contains "[exec] exited with code 0"
assert_contains "[exec-stress] completed 4/4 processes (ok=4, err=0)"
post_stress=$(awk '/^aegis> exec sh\.elf/ { exit } /\[exec-stress\] completed 4\/4 processes \(ok=4, err=0\)/ { found = 1 } found' "$log")
if ! printf '%s\n' "$post_stress" | grep -Eq 'used[[:space:]]*:[[:space:]]*0 frames'; then
    echo "FAIL: expected 0 used PMM frames after exec-stress 4" >&2
    cat "$log" >&2
    exit 1
fi
assert_contains "exec: fs: file not found"

shell_output=$(awk '
    /^Aegis EL0 shell/ { in_shell = 1 }
    in_shell { sub(/\r$/, ""); print }
    in_shell && /^\[exec\] exited with code 0$/ { exit }
' "$log")

assert_shell_line() {
    if ! printf '%s\n' "$shell_output" | grep -Fxq "$1"; then
        echo "FAIL: expected EL0 shell output missing: $1" >&2
        cat "$log" >&2
        exit 1
    fi
}

assert_shell_line "Aegis EL0 shell (sh.elf) — type 'help'"
for command in "help" "echo <text>" "cat <file>" "uptime" "run <file>" "pipetest" "exit [code]"; do
    assert_shell_line "$command"
done
assert_shell_line "hello from el0 shell"
assert_shell_line "Welcome to Aegis!"
if ! printf '%s\n' "$shell_output" | grep -Eq '^uptime: [0-9]+ ms$'; then
    echo "FAIL: expected uptime in milliseconds from the EL0 shell" >&2
    cat "$log" >&2
    exit 1
fi
assert_shell_line "Hello from EL0 userspace!"
assert_shell_line "[sh] exited with code 0"
assert_shell_line "[pipetest] ok"
assert_shell_line "[exec] exited with code 0"

post_shell_meminfo=$(awk '
    /^Aegis EL0 shell/ { saw_shell = 1 }
    saw_shell && /^\[exec\] exited with code 0/ { exited = 1 }
    exited && /^aegis> meminfo/ { in_meminfo = 1; next }
    in_meminfo { sub(/\r$/, ""); print }
    in_meminfo && /^aegis> / { exit }
' "$log")
if ! printf '%s\n' "$post_shell_meminfo" | grep -Eq '^ *used *: *0 frames$'; then
    echo "FAIL: expected 0 used PMM frames after exec sh.elf" >&2
    cat "$log" >&2
    exit 1
fi
if ! printf '%s\n' "$post_shell_meminfo" | grep -Fxq 'aegis> halt'; then
    echo "FAIL: expected halt immediately after final meminfo" >&2
    cat "$log" >&2
    exit 1
fi
assert_contains "Halting via PSCI..."

echo "PASS: QEMU v0.6.0 higher-half, SMP, PMM, and EL0 regression checks succeeded"
