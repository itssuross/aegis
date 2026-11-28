#!/usr/bin/env sh
set -e

if ! [ -f disk.img ] || [ scripts/run-qemu.sh -nt disk.img ] ||
    [ user/hello.rs -nt disk.img ] || [ user/sh.rs -nt disk.img ] ||
    [ "$(dd if=disk.img bs=8 count=1 2>/dev/null)" != AEGISFS1 ]; then
    tmpdir=$(mktemp -d ./.aegis-disk.XXXXXX)
    trap 'rm -f "$tmpdir/readme.txt" "$tmpdir/motd.txt" "$tmpdir/hello.elf" "$tmpdir/sh.elf" "$tmpdir/disk.img"; rmdir "$tmpdir"' 0

    for program in hello sh; do
        rustc --edition 2024 --target aarch64-unknown-none -C opt-level=s -C panic=abort -C strip=symbols -C link-arg=--nmagic -C link-arg=-Ttext=0x00400000 -C link-arg=-e_start -o "$tmpdir/$program.elf" "user/$program.rs"
    done
    printf '%s\n' 'Use touch, write, append, and rm to manage files in AegisFS.' > "$tmpdir/readme.txt"
    printf '%s\n' 'Welcome to Aegis!' > "$tmpdir/motd.txt"

    le32() {
        value=$1
        for shift in 0 8 16 24; do
            printf '%b' "\\0$(printf '%03o' "$(((value >> shift) & 255))")"
        done
    }

    image="$tmpdir/disk.img"
    dd if=/dev/zero of="$image" bs=512 count=512 2>/dev/null
    {
        printf 'AEGISFS1'
        le32 512
        le32 32
        le32 5
    } | dd of="$image" bs=1 conv=notrunc 2>/dev/null

    entry_offset=512
    start_sector=5
    for file in readme.txt motd.txt hello.elf sh.elf; do
        size=$(wc -c < "$tmpdir/$file")
        capacity=$(((size + 4095) / 4096 * 8))
        if [ "$capacity" -eq 0 ]; then
            capacity=8
        fi
        if [ "$((start_sector + capacity))" -gt 512 ]; then
            printf 'AegisFS: %s does not fit in disk.img\n' "$file" >&2
            exit 1
        fi
        {
            printf '%s' "$file"
            dd if=/dev/zero bs=1 count="$((48 - ${#file}))" 2>/dev/null
            le32 1
            le32 "$start_sector"
            le32 "$size"
            le32 "$capacity"
        } | dd of="$image" bs=1 seek="$entry_offset" conv=notrunc 2>/dev/null
        dd if="$tmpdir/$file" of="$image" bs=512 seek="$start_sector" conv=notrunc 2>/dev/null
        entry_offset=$((entry_offset + 64))
        start_sector=$((start_sector + capacity))
    done

    mv "$image" disk.img
    rm "$tmpdir/readme.txt" "$tmpdir/motd.txt" "$tmpdir/hello.elf" "$tmpdir/sh.elf"
    rmdir "$tmpdir"
    trap - 0
fi
exec qemu-system-aarch64 -M virt -cpu cortex-a72 -smp 4 -m 128M -nographic -serial mon:stdio -global virtio-mmio.force-legacy=false -drive file=disk.img,if=none,format=raw,cache=writethrough,id=hd0 -device virtio-blk-device,drive=hd0 -kernel "$@"
