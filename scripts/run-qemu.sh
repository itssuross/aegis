#!/usr/bin/env sh
set -e
if ! [ -f disk.img ] || ! head -c 512 disk.img | grep -aq "ustar" ||
    ! grep -aq "hello.elf" disk.img || ! grep -aq "sh.elf" disk.img ||
    [ user/hello.rs -nt disk.img ] || [ user/sh.rs -nt disk.img ] || [ scripts/run-qemu.sh -nt disk.img ]; then
    tmpdir=$(mktemp -d ./.aegis-disk.XXXXXX)
    trap 'rm -f "$tmpdir/motd.txt" "$tmpdir/readme.txt" "$tmpdir/hello.elf" "$tmpdir/sh.elf" "$tmpdir/disk.img"; rmdir "$tmpdir"' EXIT
    for program in hello sh; do
        rustc --edition 2024 --target aarch64-unknown-none -C opt-level=s -C panic=abort -C link-arg=--nmagic -C link-arg=-Ttext=0x00400000 -C link-arg=-e_start -o "$tmpdir/$program.elf" "user/$program.rs"
    done
    printf '%s\n' 'Welcome to Aegis!' > "$tmpdir/motd.txt"
    printf '%s\n' 'To pack your own files, run tar --format ustar -cf disk.img -C path/to/files .' > "$tmpdir/readme.txt"
    COPYFILE_DISABLE=1 tar --format ustar -cf "$tmpdir/disk.img" -C "$tmpdir" motd.txt readme.txt hello.elf sh.elf
    if [ "$(wc -c < "$tmpdir/disk.img")" -lt 65536 ]; then
        dd if=/dev/zero of="$tmpdir/disk.img" bs=512 count=0 seek=128 2>/dev/null
    fi
    mv "$tmpdir/disk.img" disk.img
    rm "$tmpdir/motd.txt" "$tmpdir/readme.txt" "$tmpdir/hello.elf" "$tmpdir/sh.elf"
    rmdir "$tmpdir"
    trap - EXIT
fi
exec qemu-system-aarch64 -M virt -cpu cortex-a72 -smp 4 -m 128M -nographic -serial mon:stdio -global virtio-mmio.force-legacy=false -drive file=disk.img,if=none,format=raw,id=hd0 -device virtio-blk-device,drive=hd0 -kernel "$@"
