# neochumby-kernel-sideloader: stock 2.6.16 -> NAND-less Linux 5.9.16

`neochumby-kernel-sideloader` is a static ARM EABI binary that runs on the
stock Chumby Linux 2.6.16.  It uses **CVE-2009-2698** (the NULL `struct
rtable` page-zero ELF route) to obtain uid 0 and all capabilities, maps
`/dev/mem`, stages a Linux **5.9.16** zImage and an initramfs **outside
NAND**, and hands off with the MMU off.  The new kernel is built with
`CONFIG_MTD=n`: it has no NAND driver, no `/proc/mtd`, no `/dev/mtd*`, and
no NAND platform device.

The executable is Rust.  Its ELF has a `PT_LOAD` at virtual address 0 for the
fake rtable and the jump payload; the page-zero route is the same as the
earlier C implementation, ported to `global_asm!` in `src/asm.rs`.  This is a
standalone bare-metal tool (`[workspace]` in `Cargo.toml`), so it uses
`unsafe` directly rather than living under the workspace's app-contract/unsafe
rules; it is MIT and never runs inside the neochumby jail.

The demo is for the user's own device image and emulator.

## NAND access and writes

- **Stock 2.6.16 reads NAND to boot.**  Its kernel and root disk are on the
  stock chip; there is no other first-stage boot path on the device.  That is
  a read, and QEMU opens the image with `snapshot=on` and runs with
  `-snapshot`.
- **The sideloader writes no NAND.**  The only device node it opens is
  `/dev/mem` (`O_RDWR`); the writes through it are to RAM (zImage, initrd,
  ATAGS and the handoff stub).  Those mappings are SDRAM addresses
  (`0xc2000000`, `0xc2800000`, `0xc3f00000`, `0xc3f80000`), not the NAND
  controller.  It reads the QEMU loader container, or in hardware mode
  user-supplied kernel/initramfs files read-only.  It never opens
  `/dev/mtd*`, never calls an MTD `ioctl`, and never creates MTD device nodes.
  The executable contains no `/dev/mtd`, `mtdblock`, `flash_erase`,
  `flashcp`, `nandwrite`, `mtd` or `nand` strings; `run-demo.py` checks the
  binary before boot and fails if one appears.
- **5.9.16 has no NAND driver or device.**  `CONFIG_MTD=n`; the MX21ADS board
  patch prevents a NAND platform device.  The new OS has no `/proc/mtd`, no
  `/dev/mtd*`, no `/sys/class/mtd`, and no NAND in dmesg.
- **The demo has one host-side NAND-image preparation step.**  `mk_psp_nand.py`
  copies the sanitized original into a separate `nand-scratch.img` and writes
  the PSP partition in that copy, so the stock boot can find the sideloader in
  `/psp/rfs1/rcS`.  It is not a guest/MTD/device write, it never opens the
  original for writing, and the original SHA-256 is checked before and after.
  If host image preparation itself is out of scope, use a pre-made scratch
  image or a real USB stick; the runtime path (the sideloader and 5.9.16)
  still performs zero NAND writes.
- The stock rcS has later `flash_eraseall`/`nandwrite` lines, but the
  `/psp/rfs1/rcS` hook exits before them; QEMU snapshots any incidental stock
  writes.

Audit of the tree:

| path | NAND/MTD use |
| --- | --- |
| `src/main.rs`, `src/container.rs` | opens `/dev/mem` only; maps SDRAM; no MTD operation or MTD literals |
| `src/asm.rs` | page-zero entry and handoff payloads; no device access |
| `run-demo.py` | stock root cmdline `/dev/mtdblock5` (read-only mount); scratch `if=mtd` with `snapshot=on`; static/guest MTD checks |
| `mk_psp_nand.py` | host-side scratch image copy; input opened read-only |
| `kernel/nonand.config`, `kernel/*.patch` | `CONFIG_MTD=n`; no NAND platform device on MX21ADS |
| `kernel/build.sh` | downloads/builds kernel; no NAND access |

## Result

`evidence/sideloader-demo.log` is one serial stream.  It contains the stock
2.6.16 boot, the loader messages, the 5.9.16 boot log, the initramfs `/init`,
and an interactive shell.  The log is from the build that was then named
`chumpwn`; its lines still use that prefix.  The key lines:

```text
chumpwn: start uid=0
chumpwn: first trigger r=0
chumpwn: uid after=0
chumpwn: caps eff=0xffffffff
chumpwn: caps permitted=0xffffffff
chumpwn: caps inheritable=0xffffffff
chumpwn: second trigger -> 0xc3f80000
[    0.000000] Booting Linux on physical CPU 0x0
[    0.000000] Linux version 5.9.16-mx21-nomtd ...
...
### chumpwn: Linux 5.9.16 userspace reached ###
Linux (none) 5.9.16-mx21-nomtd #1 PREEMPT ... armv5tejl GNU/Linux
--- /proc/mtd ---
absent: no /proc/mtd
--- /dev/mtd* ---
absent: no /dev/mtd*
--- dmesg | grep -i nand ---
absent: no NAND in dmesg
--- NAND platform devices ---
absent: no NAND platform device
...
CHUMPWN_SHELL_OK
```

`evidence/run-demo.out` also records the NAND hashes before/after:

```text
original NAND before: 5779fd4223e09f0e96eea13296f7147cf1e4baa90632368a0f5743504129db86
original NAND after:  5779fd4223e09f0e96eea13296f7147cf1e4baa90632368a0f5743504129db86
scratch NAND before:  c1603f7a76bb969b290169d9b6549ba739848526edb396a86d9f21e9631bdfc1
scratch NAND after:   c1603f7a76bb969b290169d9b6549ba739848526edb396a86d9f21e9631bdfc1
```

## Layout

The i.MX21's 64 MiB SDRAM is at `0xc0000000`.  The stock 2.6.16 kernel is
started by QEMU in the normal way (`-kernel stock-zImage`) and still reads the
stock NAND to mount its root disk.  That first stage is unavoidable: the
stock boot loader/root disk has no other boot path.

The 5.9.16 payload is placed by QEMU, not NAND:

| item | physical | how |
| --- | --- | --- |
| stock 2.6.16 zImage | QEMU load address | `-kernel`; stock kernel reads NAND |
| loader container | `0xc2000000` | `-device loader,file=payload.bin,addr=0xc2000000` |
| 5.9.16 zImage | `0xc2000000` | copied forward over the container header; source is byte-offset `+0x1000` |
| initramfs | `0xc2800000` | copied from the container |
| ATAGS | `0xc3f00000` | built by the sideloader (same address as the earlier C chainload) |
| handoff stub | `0xc3f80000` | copied by the sideloader (same address as the earlier C chainload) |

The container at `0xc2000000` is still free when stock `/psp/rfs1/rcS` runs;
this was checked by loading the full container before boot and reading it
back from rcS, both with the demo's 5.2 MB container and with a 6.4 MB
`dist/`-built one.  It is inside the stock kernel's 64 MiB direct map, so the
stub at `0xc3f80000` executes with the MMU on and continues at the same
physical address after the MMU is disabled.

The stock kernel does **not** see the container as NAND or as an initramfs; it
is just RAM written by QEMU's loader.  The sideloader does not open `/dev/mtd*`
at all.

## Relationship to the earlier C implementation

The Rust follows the existing, verified C sources; the table records the
correspondence.

| Rust | C source | Notes |
| --- | --- | --- |
| `link.ld` | `poc2/link.ld` | `PT_LOAD` at vaddr 0 for `.zero`; code at `0x10000` |
| `kernel_payload` in `src/asm.rs` | `stage2/payload.S`, `poc2/payload.S` | same uid/gid scan, same capability offsets `+0x24/+0x28/+0x2c` |
| `trigger()` in `src/trigger.rs` | `chainload.c trigger_poc()`, `stage2/exploit.c` | same `socket(AF_INET, SOCK_DGRAM)`, two `sendmsg`s, `MSG_MORE\|MSG_PROBE` then `0` |
| `0x74`, `0x100`, `0x104` writes | `chainload.c` / `exploit.c` | fake `rtable.output` and uid/gid values |
| `build_atags()` in `src/atags.rs` | `chainload.c build_atags()` | same ATAG_CORE/MEM/CMDLINE/NONE; added ATAG_INITRD2 for the separate initramfs |
| `map_phys()`, `cacheflush()` in `src/trigger.rs` | `chainload.c` | same `mmap2`/`MAP_FIXED` and ARM private `0x0f0002` |
| `jump_blob` in `src/asm.rs` | `chainload-chumby/jump.S` | same I-cache/TLB invalidate and jump |
| `stub_blob` in `src/asm.rs` | `chainload-chumby/stub.S` | same D-cache clean, MMU off, `r0=0`, `r1=851`, `r2=ATAGS`, `pc=zImage` |
| `sideloader_main()` in `src/main.rs` | `chainload.c _start()` | same drop-to-1000, first trigger, capget, `/dev/mem`, staging, second trigger |

Differences are deliberate and limited to the NAND-less requirement:

- The zImage comes from a QEMU loader container at `0xc2000000` (or USB files),
  not from `/dev/mtdblock6`.
- The initramfs is a separate file at `0xc2800000`, so `ATAG_INITRD2` is added
  and the cache flush covers it.
- ATAGS and stub use the earlier C chainload's addresses
  `0xc3f00000`/`0xc3f80000`.
- The command line has no `mtdparts=`, because the 5.9.16 kernel has no MTD.

## Build

The Chumby is ARM926EJ-S, EABI5, soft-float.  The Rust target is
`armv5te-unknown-linux-musleabi`; rustc's `rust-lld` links the custom page-zero
linker script (the crate has its own `.cargo/config.toml`, so the repository's
zig linker wrapper is not used).

```sh
rustup target add armv5te-unknown-linux-musleabi
cd tools/neochumby-kernel-sideloader
./build.sh                 # -> tools/neochumby-kernel-sideloader/neochumby-kernel-sideloader
```

### Linux 5.9.16

`kernel/build.sh` downloads the official 5.9.16 tarball (SHA-256 in the
script), applies `kernel/nonand.config`, applies a two-line MX21ADS patch so
the board file does not register a NAND platform device when
`CONFIG_MTD_NAND_MXC` is absent, and builds the zImage.  On macOS it also
supplies a host `elf.h` and a `uuid_t` workaround for
`scripts/mod/file2alias.c`; those are build-host issues, not kernel changes.

```sh
./kernel/build.sh
# -> build/linux-5.9.16-mx21-nomtd-zImage
```

Relevant final config (full list in `evidence/kernel-config.txt`):

```text
# CONFIG_MTD is not set
# CONFIG_MODULES is not set
CONFIG_LOCALVERSION="-mx21-nomtd"
CONFIG_INITRAMFS_SOURCE=""
CONFIG_CMDLINE="console=ttymxc0,115200 rdinit=/init"
```

The `IMX_HAVE_PLATFORM_MXC_NAND` helper can still be built for other i.MX
machines, but the MX21ADS board file is guarded by `CONFIG_MTD_NAND_MXC` and
does not register a NAND device.

### Userspace initramfs

The initramfs is `dist/`'s: the Buildroot external tree (`../../dist`) that
builds the Chumby's own mainline userspace, not a placeholder.  `dist/build`
runs Buildroot in a container and writes a zImage and a gzipped newc cpio;
either can stand in for `kernel/build.sh`'s zImage above, since the
sideloader only cares that the file starts with a zImage magic and that the
initramfs is a gzipped cpio.

```sh
../../dist/build build      # -> build/zImage, build/rootfs.cpio.gz
python3 mkpayload.py build/zImage build/rootfs.cpio.gz build/payload.bin
```

`dist/`'s kernel config also sets `CONFIG_MTD=n` (see
`dist/board/chumby-classic/linux.fragment`), so the no-NAND guarantee holds
either way.  `dist/README.md` lists what's in its userspace so far and what's
still to come (`neochumby-init`, the Chumby's own devices).

## Run in QEMU

`run-demo.py` prepares a separate scratch NAND image from the sanitized
original, replacing only the 2 MiB PSP partition with a JFFS2 tree containing
the sideloader and `rfs1/rcS` (see the no-write section above).  It then
starts QEMU with the original NAND read-only, the payload via
`-device loader`, and an interactive serial socket; it sends the NAND checks
and shell markers, checks the sideloader binary for MTD strings, fails if
`/proc/mtd`, `/dev/mtd*`, `/sys/class/mtd` or NAND in dmesg appears, and fails
if either the original or the scratch image hash changes.

```sh
QEMU=/path/to/qemu-system-arm \
STOCK_ZIMAGE=/path/to/stock/zImage \
NAND_SRC=/path/to/firmware/nand.img \
CHUMBY_EMU=/path/to/chumby-emu \
python3 tools/neochumby-kernel-sideloader/run-demo.py
```

The exact QEMU command used for the captured log is in
`evidence/qemu-command.txt`; the essential part is:

```sh
qemu-system-arm -M chumby-classic \
  -kernel "$STOCK_ZIMAGE" \
  -drive "if=mtd,file=$WORK/nand-scratch.img,format=raw,snapshot=on" \
  -device loader,file="$PAYLOAD",addr=0xc2000000,force-raw=on \
  -snapshot -display none -no-reboot -monitor none \
  -serial file:"$LOG" \
  -append 'console=ttyS0,38400 root=/dev/mtdblock5 rootfstype=cramfs init=/linuxrc chumby_hwversion=3.8' \
  -icount shift=auto,sleep=off
```

The stock kernel's command line is unchanged except that it still gets its
normal `console=ttyS0,38400` and NAND root.  The 5.9.16 kernel gets its own
ATAGS command line from the sideloader: `console=ttymxc0,115200 rdinit=/init`.

## Files

```text
src/main.rs             entry, option parsing, loader flow
src/asm.rs              page-zero entry, kernel payload, jump and stub blobs
src/atags.rs            ATAGS builder
src/container.rs        QEMU loader container parser
src/layout.rs           SDRAM addresses and default command line
src/runtime.rs          mem* helpers and volatile copies
src/sys.rs              syscalls, printing, string helpers, file reads
src/trigger.rs          CVE-2009-2698 trigger, mmap and cacheflush
link.ld                 page-zero PT_LOAD at 0, code at 0x10000
build.sh                cargo build + copy the binary
mkpayload.py            QEMU loader container
mk_psp_nand.py          host-side scratch NAND: replace only the PSP partition
run-demo.py             QEMU boot, serial interaction, NAND hashes
kernel/build.sh         5.9.16 NAND-less zImage build
kernel/nonand.config    CONFIG_MTD=n and friends
kernel/*.patch          MX21ADS NAND platform guard; macOS host uuid workaround
evidence/               captured logs, config, hashes, exact command
```

## USB / hardware mode

On a real Chumby (or a future QEMU with USB storage), the sideloader can read
the zImage and initramfs directly instead of using the QEMU loader container:

```sh
/psp/neochumby-kernel-sideloader --kernel /mnt/usb/sideloader/zImage \
                                 --initramfs /mnt/usb/sideloader/initramfs.cpio.gz
```

That mode is implemented (`--kernel`, `--initramfs`) but was not exercised in
QEMU, because `chumby-classic` models the USB root hub but no storage devices.
The QEMU demo uses `--source 0xc2000000`, the loader container.

## Limitations / notes

- **Stock 2.6.16 first stage reads NAND.**  The stock kernel and root disk are
  on NAND and that is the only boot path QEMU/the device has.  The new OS is
  the part with no NAND driver.
- **QEMU only.**  QEMU's `chumby-classic` models no USB storage, so the
  hardware-oriented `--kernel FILE --initramfs FILE` mode is implemented but
  the demo uses the QEMU loader container.  A real Chumby would need the files
  on a USB stick and the stock USB storage driver.
- **5.9.16 is EOL.**  i.MX21 was removed in 5.10; 5.9.16 is the last mainline
  with it.  The kernel is a local 5.9.16 build with the NAND-less config and
  the MX21ADS NAND-platform guard, not a vendor kernel.
- The loader region at `0xc2000000` is free at rcS for this stock image.  A
  different stock userland could allocate it before rcS; the QEMU demo checks
  this by loading the full container and reading it back.  `SRC_MAP_LEN`
  (the window the sideloader reads the container through) is `0x700000`, matching
  `mkpayload.py`'s own `LIMIT`; a container that fits under `LIMIT` always
  fits the read.
- The page-zero ELF route is intentional and only works on the vulnerable
  stock 2.6.16.  If the sideloader is run inside the neochumby jail, the
  `jail/refuse-page-zero-elfs` policy may refuse the ELF; it is meant to run
  from `/psp/rfs1/rcS`, a shell, or a USB stick, outside the jail.
- **Next step:** `dist/` still needs `neochumby-init` and the Chumby's own
  devices (see `dist/README.md`); the sideloader itself needs no change as
  that userspace grows, since only the cpio contents change.
