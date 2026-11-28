#!/usr/bin/env sh
set -e

cargo build
cargo build --release

kernel="${CARGO_TARGET_DIR:-target}/aarch64-unknown-none/debug/aegis"
case "$kernel" in
    /*) ;;
    *) kernel="$PWD/$kernel" ;;
esac

tmpdir=$(mktemp -d)
fifo="$tmpdir/qemu.in"
qemu_pid=""
feeder_pid=""
watchdog_pid=""

stop_job() {
    if [ -n "$1" ]; then
        pkill -P "$1" 2>/dev/null || true
        kill "$1" 2>/dev/null || true
        wait "$1" 2>/dev/null || true
    fi
}

cleanup() {
    stop_job "$watchdog_pid"
    stop_job "$feeder_pid"
    stop_job "$qemu_pid"
    rm -f "$tmpdir/boot1.log" "$tmpdir/boot2.log" "$fifo" "$tmpdir/disk.img" \
        "$tmpdir/scripts" "$tmpdir/user"
    rmdir "$tmpdir"
}
trap cleanup EXIT
trap 'exit 1' INT TERM

ln -s "$PWD/scripts" "$tmpdir/scripts"
ln -s "$PWD/user" "$tmpdir/user"
mkfifo "$fifo"

fail() {
    printf 'FAIL: %s\n' "$1" >&2
    cat "$log" >&2
    exit 1
}

assert_contains() {
    grep -Fq "$1" "$log" || fail "expected output missing: $1"
}

assert_line() {
    tr -d '\r' < "$log" | grep -Fxq "$1" || fail "expected line missing: $1"
}

wait_for_log() {
    ticks=0
    while [ "$ticks" -lt 150 ]; do
        if [ "$(grep -Fc "$1" "$log")" -ge "${2:-1}" ]; then
            return 0
        fi
        sleep 0.1
        ticks=$((ticks + 1))
    done
    printf 'Timed out waiting for %s\n' "$1" >&2
    return 1
}

run_command() {
    wait_for_log "aegis> " "$prompt"
    printf '%s\n' "$1"
    prompt=$((prompt + 1))
}

boot_one() {
    run_command "fsinfo"
    run_command "write persist.txt hello-aegis-fs"
    run_command "append persist.txt -v2"
    run_command "cat persist.txt"
    run_command "halt"
}

boot_two() {
    run_command "ls"
    run_command "cat persist.txt"
    run_command "rm persist.txt"
    run_command "ls"

    for cmd in "info" "meminfo" "cpuinfo" "ps" "blkinfo" "cat motd.txt" "exec hello.elf"; do
        run_command "$cmd"
    done

    wait_for_log "[exec] exited with code 0"
    run_command "exec-stress 4"
    wait_for_log "[exec-stress] completed 4/4 processes (ok=4, err=0)"

    run_command "meminfo"
    wait_for_log "used      : 0 frames" 2
    run_command "exec missing.elf"
    wait_for_log "exec: fs: file not found"
    run_command "exec sh.elf"

    shell_prompt=1
    for cmd in \
        "help" \
        "echo hello from el0 shell" \
        "cat motd.txt" \
        "touch el0note.txt" \
        "fsinfo" \
        "append el0note.txt contents-that-touch-must-preserve" \
        "touch el0note.txt" \
        "cat el0note.txt" \
        "write el0note.txt hello-from-el0" \
        "append el0note.txt -userspace" \
        "cat el0note.txt" \
        "rm el0note.txt" \
        "cat el0note.txt" \
        "rm el0note.txt" \
        "uptime" \
        "run hello.elf" \
        "pipetest" \
        "exit"
    do
        wait_for_log "sh> " "$shell_prompt"
        printf '%s\n' "$cmd"
        shell_prompt=$((shell_prompt + 1))
    done

    wait_for_log "[exec] exited with code 0" 2
    run_command "meminfo"
    wait_for_log "used      : 0 frames" 3
    run_command "halt"
}

run_boot() {
    log="$tmpdir/$1.log"
    : > "$log"
    (
        cd "$tmpdir"
        exec ./scripts/run-qemu.sh "$kernel"
    ) < "$fifo" > "$log" 2>&1 &
    qemu_pid=$!

    (
        prompt=1
        "$2"
    ) > "$fifo" &
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

    if wait "$qemu_pid"; then
        qemu_status=0
    else
        qemu_status=$?
    fi
    qemu_pid=""
    stop_job "$watchdog_pid"
    watchdog_pid=""
    if [ "$qemu_status" -ne 0 ]; then
        fail "QEMU exited with status $qemu_status"
    fi
    if ! wait "$feeder_pid"; then
        feeder_pid=""
        fail "command feeder exited early"
    fi
    feeder_pid=""

    assert_contains "[fs] mounted AegisFS"
    assert_contains "Halting via PSCI..."
    if grep -Fq "warning:" "$log"; then
        fail "unexpected warning"
    fi
}

run_boot boot1 boot_one
assert_contains "AegisFS: 512 sectors"
assert_line "hello-aegis-fs-v2"

run_boot boot2 boot_two
assert_line "hello-aegis-fs-v2"
if ! awk '
    { sub(/\r$/, "") }
    /^aegis> / {
        listing = ($0 == "aegis> ls")
        if (listing) ++lists
    }
    listing && $1 == "FILE" && $3 == "persist.txt" { size[lists] = $2 }
    END { exit !(lists == 2 && size[1] == 17 && !(2 in size)) }
' "$log"; then
    fail "expected persist.txt in the first listing only"
fi

assert_contains "active cores: 4/4"
assert_line "Aegis v0.7.0 (aarch64-unknown-none)"
assert_contains "KernelVMA  : 0xFFFF_FF80_4008_0000 (TTBR1_EL1)"
assert_contains "[pmm]"
assert_contains "0 frames leaked"
assert_contains "managed   :"
assert_contains "online: 4/4"
assert_contains "sectors  : 512 (512 B each)"
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
for command in "help" "echo <text>" "fsinfo" "cat <file>" "touch <file>" \
    "write <file> <text>" "append <file> <text>" "rm <file>" \
    "uptime" "run <file>" "pipetest" "exit [code]"; do
    assert_shell_line "$command"
done
assert_shell_line "hello from el0 shell"
assert_shell_line "Welcome to Aegis!"
if ! printf '%s\n' "$shell_output" | grep -Eq '^AegisFS: 512 sectors \([0-9]+ used, [0-9]+ free\), 5/32 files$'; then
    fail "expected EL0 fsinfo to count the file created by touch"
fi
assert_shell_line "contents-that-touch-must-preserve"
assert_shell_line "hello-from-el0-userspace"
assert_shell_line "cat: cannot open el0note.txt (-2)"
assert_shell_line "rm: cannot remove el0note.txt (-2)"
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
echo "PASS: AegisFS writes, reboot persistence, deletion, and QEMU higher-half, SMP, PMM, and EL0 checks"
