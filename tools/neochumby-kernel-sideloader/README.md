# neochumby-kernel-sideloader: stock 2.6.16 -> sideload Linux 5.9.16

`neochumby-kernel-sideloader` is a static ARM EABI binary that runs on the
stock Chumby Linux 2.6.16.  It uses **CVE-2009-2698** (the NULL `struct
rtable` page-zero ELF route) to obtain uid 0 and all capabilities, maps
`/dev/mem`, stages a Linux **5.9.16** zImage and an initramfs **outside
NAND**, and hands off with the MMU off.

The image is **sideload**: the kernel has no NAND driver built in.  The NAND
drivers are modules in the initramfs, and `/init` loads them only when the
kernel was booted without the sideloader's `neochumby.sideloaded=1` marker
— that is, when the image was flashed instead of handed off.  A normal
sideload never loads them, so `/proc/mtd`, `/dev/mtd*`, `/sys/class/mtd` and
NAND in dmesg stay absent.

The executable is Rust.  Its ELF has a `PT_LOAD` at virtual address 0 for the
fake rtable and the jump payload; the page-zero route is the same as the
earlier C implementation, ported to `global_asm!` in `src/asm.rs`.  This is a
standalone bare-metal tool (`[workspace]` in `Cargo.toml`), outside the
workspace's app contract, but it follows the same unsafe-code rule: the raw
layer is isolated and documented, and the rest of the crate is safe.  It is
MIT and never runs inside the neochumby jail.

The demo is for the user's own device image and emulator.

## Unsafe code

The crate denies `unsafe_code` and `unsafe_op_in_unsafe_fn`.  The only Rust
`unsafe` is in `src/raw.rs`, which holds the raw Linux 2.6.16 system calls,
the volatile memory access and the entry ABI.  Every `unsafe` block has a
`// SAFETY:` comment, and the rest of the crate uses `raw::Mem` (a
bounds-checked mapping) and the safe wrappers.  `src/asm.rs` is the assembly
payload; `global_asm!` is flagged by the `unsafe_code` lint even though it
contains no Rust `unsafe`.

This is the same pattern as the workspace's `neochumby-sys`: the raw layer is
the only place that can be unsafe, and its callers are safe.  A bootloader
cannot avoid it entirely — it makes system calls directly and runs with the
MMU off — but it can keep the unsafe surface small and reviewable.

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
  `flashcp` or `nandwrite` strings;
  `run-demo.py` checks the binary before boot and fails if one appears.
- **5.9.16 is sideload by default.**  The kernel has no NAND driver built in;
  the NAND drivers are modules in the initramfs.  `/init` loads them only
  when the sideload marker is absent (the flashed case).  In the normal
  sideloaded path there is no `/proc/mtd`, no `/dev/mtd*`, no
  `/sys/class/mtd`, and no NAND in dmesg.
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
| `kernel/sideload-only.config`, `kernel/*.patch` | sideload-only build: `CONFIG_MTD=n`; no NAND platform device on MX21ADS |
| `kernel/build.sh` | downloads/builds the sideload-only zImage; no NAND access |
| `dist/board/chumby-classic/linux.fragment` | sideload recovery build: MTD/NAND as modules |
| `dist/board/chumby-classic/rootfs/init` | loads the NAND modules only when the sideload marker is absent |

## Result

`evidence/sideloader-demo.log` is one serial stream.  It contains the stock
2.6.16 boot, the sideloader, the 5.9.16 boot with the built-in initramfs, the
`sideload: sideloaded` marker check, the NAND checks, and an interactive
shell.  The key lines:

```text
sideloader: no NAND access in this path
sideloader: start uid=0
sideloader: first trigger r=0
sideloader: uid after=0
sideloader: caps eff=0xffffffff
sideloader: caps permitted=0xffffffff
sideloader: caps inheritable=0xffffffff
sideloader: second trigger -> 0xc3f80000
[    0.000000] Booting Linux on physical CPU 0x0
[    0.000000] Linux version 5.9.16-mx21-sideload ...
...
### sideload: Linux 5.9.16 initramfs ###
Linux (none) 5.9.16-mx21-sideload #3 PREEMPT ... armv5tejl GNU/Linux
--- /proc/cmdline ---
console=ttymxc0,115200 rdinit=/init neochumby.sideloaded=1
sideload: sideloaded; NAND modules stay unloaded
--- /proc/mtd ---
absent: no /proc/mtd
--- /dev/mtd* ---
absent: no /dev/mtd*
--- dmesg | grep -iE 'nand:|mxc_nand|nand device|nand flash' ---
NAND_GREP_RC=1
...
SIDELOADER_SHELL_OK
```

`evidence/run-demo.out` also records the NAND hashes before/after:

```text
original NAND before: 5779fd4223e09f0e96eea13296f7147cf1e4baa90632368a0f5743504129db86
original NAND after:  5779fd4223e09f0e96eea13296f7147cf1e4baa90632368a0f5743504129db86
scratch NAND before:  ff57973ffa2d9c47c015fe01d0733b015fefdce955536504693df8284f64b773
scratch NAND after:   ff57973ffa2d9c47c015fe01d0733b015fefdce955536504693df8284f64b773
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
| loader container | `0xc2000000` | `-device loader,file=sideload-payload.bin,addr=0xc2000000` |
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

`kernel/build.sh` builds the **sideload-only** sideload zImage.  It downloads
the official 5.9.16 tarball (SHA-256 in the script), applies
`kernel/sideload-only.config`, applies a two-line MX21ADS patch so the
board file does not register a NAND platform device when
`CONFIG_MTD_NAND_MXC` is absent, and builds the zImage.  It has no built-in
initramfs and no recovery modules: it is for the sideloader only, not for
flashing.  On macOS it also supplies a host `elf.h` and a `uuid_t` workaround
for `scripts/mod/file2alias.c`; those are build-host issues, not kernel
changes.

```sh
./kernel/build.sh
# -> build/linux-5.9.16-mx21-sideload-only-zImage
```

Relevant final config (full list in `evidence/kernel-config.txt`):

```text
# CONFIG_MTD is not set
# CONFIG_MODULES is not set
CONFIG_LOCALVERSION="-mx21-sideload-only"
CONFIG_INITRAMFS_SOURCE=""
CONFIG_CMDLINE="console=ttymxc0,115200 rdinit=/init"
```

The `IMX_HAVE_PLATFORM_MXC_NAND` helper can still be built for other i.MX
machines; the sideload-only build guards the MX21ADS NAND device off, while
the recovery build below registers it so the NAND module can bind.

### Userspace initramfs

The initramfs is `dist/`'s: the Buildroot external tree (`../../dist`) that
builds the Chumby's own mainline userspace.  `dist/build` runs Buildroot in a
container and writes the **sideload recovery** image: the zImage has the
initramfs built in, so it is self-contained when flashed and when handed off
by the sideloader.  The same initramfs is also written beside it for
userhook2 or another kexec path.  The kernel has no NAND driver built in; the
NAND modules are in the initramfs, and `/init` loads them only when the
sideload marker is absent (the flashed case).

```sh
../../dist/build build      # -> build/sideload-zImage, build/sideload-rootfs.cpio.gz
python3 mkpayload.py --no-initramfs build/sideload-zImage build/sideload-payload.bin
```

The `--no-initramfs` container tells the sideloader to use the zImage's
built-in initramfs; the zImage plus the external cpio would not fit the
7 MiB container window.  `sideload-rootfs.cpio.gz` is for userhook2 and
other boot paths that take an initramfs separately.

`dist/`'s kernel config sets the NAND drivers as modules and
`CONFIG_LOCALVERSION="-mx21-sideload"` (see
`dist/board/chumby-classic/linux.fragment`), so the normal sideloaded path
never loads them, and a flashed image can still recover.  `dist/README.md`
lists what's in its userspace so far and what's still to come
(`neochumby-init`, the Chumby's own devices).

## Run in QEMU

Both demos run on qemu-chumby's `chumby-classic` machine (the submodule
`emu/qemu/qemu-chumby`, built by `emu/qemu/build`), started and driven
through its runner, `chumby-machine`, as all of neochumby's emulation is.
`QEMU` names the `qemu-system-arm` to use (else `emu/qemu/build`'s, or the
one on `PATH`).

`run-demo.py` prepares a separate scratch NAND image from the sanitized
original, replacing only the 2 MiB PSP partition with a JFFS2 tree containing
the sideloader and `rfs1/rcS` (see the no-write section above).  It then
starts the machine with the scratch NAND image (`snapshot=on`) and the
payload via `-device loader`; it runs the NAND checks
and shell markers, checks the sideloader binary for MTD device paths and
write tools, checks that the sideload marker is present and the NAND modules
are not loaded, fails if `/proc/mtd`, `/dev/mtd*`, `/sys/class/mtd` or NAND
in dmesg appears, and fails if either the original or the scratch image hash
changes.

```sh
STOCK_ZIMAGE=/path/to/stock/zImage \
NAND_SRC=/path/to/firmware/nand.img \
PAYLOAD=build/sideload-payload.bin \
python3 tools/neochumby-kernel-sideloader/run-demo.py
```

The exact QEMU command used for the captured log (before the demos moved
onto `chumby-machine`, which adds its console and QMP sockets and `-nic
none`) is in `evidence/qemu-command.txt`; the essential part is:

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
ATAGS command line from the sideloader:
`console=ttymxc0,115200 rdinit=/init neochumby.sideloaded=1`.

To exercise the recovery path, boot `sideload-zImage` directly with no
external initramfs and no marker.  Its built-in initramfs sees no marker,
loads the NAND modules, and prints `/proc/mtd`:

```sh
qemu-system-arm -M chumby-classic \
  -kernel build/sideload-zImage \
  -drive "if=mtd,file=$WORK/nand-scratch.img,format=raw,snapshot=on" \
  -snapshot -display none -no-reboot -monitor none \
  -serial file:"$WORK/sideload-flashed.log" \
  -append 'console=ttymxc0,115200 rdinit=/init' \
  -icount shift=auto,sleep=off
```

`run-recovery-demo.py` automates that check with a synthetic zeroed NAND
image, so it needs no stock firmware:

```sh
KERNEL=build/sideload-zImage \
python3 tools/neochumby-kernel-sideloader/run-recovery-demo.py
```

It fails unless the log shows the no-marker path, the NAND modules loading,
`mtd0:`, `/dev/mtdblock0`, `mxc_nand` in `/proc/modules`, and the recovery
shell.  The sideloaded path is `run-demo.py`, which needs the user's own
stock zImage and NAND image.

CI's `sideload` job (`.github/workflows/ci.yml`) runs this recovery test,
with the custom QEMU and the image from the `dist` job.  It runs on changes
under `dist/`, `tools/neochumby-kernel-sideloader/` or `emu/qemu/`, and on
`workflow_dispatch`, and is skipped otherwise; QEMU and Buildroot work is
cached, so the first run is slow and later runs rebuild only what changed.

## Files

```text
src/main.rs             option parsing and loader flow (safe)
src/asm.rs              page-zero entry, kernel payload, jump and stub blobs
src/atags.rs            ATAGS builder
src/container.rs        QEMU loader container parser
src/layout.rs           SDRAM addresses and the sideload sideload cmdline
src/raw.rs              raw syscalls, volatile memory, entry ABI (the only unsafe Rust)
src/sys.rs              syscall numbers, printing, string helpers, file reads
src/trigger.rs          CVE-2009-2698 trigger sequence
link.ld                 page-zero PT_LOAD at 0, code at 0x10000
build.sh                cargo build + copy the binary
mkpayload.py            QEMU loader container
mk_psp_nand.py          host-side scratch NAND: replace only the PSP partition
run-demo.py             the stock boot to the sideload, on chumby-machine; sideload checks
run-recovery-demo.py    self-contained recovery-path E2E on chumby-machine (zeroed NAND, no firmware)
kernel/build.sh         sideload-only 5.9.16 zImage build
kernel/sideload-only.config  CONFIG_MTD=n and friends
kernel/*.patch          MX21ADS NAND platform guard; macOS host uuid workaround
evidence/               captured logs, config, hashes, exact command
```

## USB / hardware mode

On a real Chumby (or the emulated one, with a stick on its USB port), the
sideloader can read the zImage and initramfs directly instead of using the
QEMU loader container:

```sh
/psp/neochumby-kernel-sideloader --kernel /mnt/usb/sideloader/zImage \
                                 --initramfs /mnt/usb/sideloader/initramfs.cpio.gz
```

That mode is implemented (`--kernel`, `--initramfs`) but not yet exercised
in QEMU: `chumby-classic` now attaches a USB stick (`chumby-machine start
--stick IMG`), which the stock boot mounts on `/mnt/usb`, but the demo still
uses `--source 0xc2000000`, the loader container.

## Limitations / notes

- **Stock 2.6.16 first stage reads NAND.**  The stock kernel and root disk are
  on NAND and that is the only boot path QEMU/the device has.  The 5.9.16
  image is sideload by default: no NAND driver is built in, and the recovery
  modules are not loaded in the sideloaded path.
- **QEMU only.**  The hardware-oriented `--kernel FILE --initramfs FILE`
  mode is implemented, but the demo uses the QEMU loader container.  A real
  Chumby needs the files on a USB stick and the stock USB storage driver,
  as the emulated one now has.
- **5.9.16 is EOL.**  i.MX21 was removed in 5.10; 5.9.16 is the last mainline
  with it.  The kernel is a local 5.9.16 build with the sideload config
  (NAND drivers as modules, not built in), not a vendor kernel.
- **The recovery modules are a deliberate trade-off.**  The image carries the
  NAND drivers in the initramfs so a mistaken flash is recoverable.  `/init`
  does not load them when `neochumby.sideloaded=1` is present, but root can
  load them deliberately.  The normal sideloaded path never does.
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
