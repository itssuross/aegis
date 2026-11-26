#!/usr/bin/env sh
set -e
if ! [ -f disk.img ] || ! head -c 512 disk.img | grep -aq "ustar" || ! grep -aq "hello.elf" disk.img || [ user/hello.rs -nt disk.img ]; then
    tmpdir=$(mktemp -d)
    trap 'rm -f "$tmpdir/motd.txt" "$tmpdir/readme.txt" "$tmpdir/hello.elf"; rmdir "$tmpdir"' EXIT
    rustc --edition 2021 --target aarch64-unknown-none -C opt-level=s -C panic=abort -C link-arg=--nmagic -C link-arg=-Ttext=0x42000000 -C link-arg=-e_start -o "$tmpdir/hello.elf" user/hello.rs
    printf '%s\n' 'Welcome to Aegis v0.2.0!' > "$tmpdir/motd.txt"
    printf '%s\n' 'To pack your own files, run tar --format ustar -cf disk.img -C path/to/files .' > "$tmpdir/readme.txt"
    COPYFILE_DISABLE=1 tar --format ustar -cf disk.img -C "$tmpdir" .
    rm "$tmpdir/motd.txt" "$tmpdir/readme.txt" "$tmpdir/hello.elf"
    rmdir "$tmpdir"
    trap - EXIT
    dd if=/dev/zero of=disk.img bs=512 count=0 seek=128 2>/dev/null
fi
exec qemu-system-aarch64 -M virt -cpu cortex-a72 -m 128M -nographic -serial mon:stdio -global virtio-mmio.force-legacy=false -drive file=disk.img,if=none,format=raw,id=hd0 -device virtio-blk-device,drive=hd0 -kernel "$@"
