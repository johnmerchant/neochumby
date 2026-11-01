# neochumby-kernel-sideloader: from the stock 2.6.16 to Linux 7

Linux 7 is the only kernel neochumby runs on, and it reaches it from the stock
firmware, which it leaves as it is. `sideload.ko` is a 2.6.16 kernel module,
loaded by root on the stock system: the module reserves four 4 MiB blocks in
RAM, root writes the zImage and initramfs through `/proc`, and a final write
builds the ATAGS, services the watchdog, flushes the caches and jumps with the
MMU off.

The zImage and initramfs are `dist/build`'s; they have no NAND driver
(`CONFIG_MTD=n`). A power cycle returns to the stock firmware; the stock NAND
is unchanged.

## How it works

The module takes four order-10 blocks (4 MiB each), sorts them by address,
and uses:

| block | use |
| --- | --- |
| `blocks[0]` | guard |
| `blocks[1]` | unused |
| `blocks[2]` | zImage (must stay above the decompressed Image) |
| `blocks[3]` | initramfs |

The stock page allocator owns them until the jump.  root writes:

- `/proc/sideload-kernel` — the zImage, up to 4 MiB;
- `/proc/sideload-initrd` — the initramfs, up to 4 MiB;
- `/proc/sideload-go` — starts the handoff.

`/proc/sideload-reserve` prints the kernel and initrd physical addresses.
The zImage is in the third block (12 MiB) and the initrd in the fourth
(16 MiB), so the decompressed Image can use the space between the kernel
start and the zImage.

The handoff builds ATAGS at `0xc3f00000`, copies the position-independent
stub to `0xc3f80000`, writes the watchdog's timeout and service registers,
disables interrupts, flushes the ARM926 caches, and jumps with the MMU off.
The target kernel's `imx2-wdt` driver then services the watchdog.

Linux 7 can't read the settings partition, so what it needs of `/psp` goes
with the initramfs: `crates/neochumby-handoff` writes the initramfs, then
`/psp`'s files as a second archive, to `/proc/sideload-initrd`.

## Build

```sh
tools/neochumby-kernel-sideloader/build OUT    # OUT/sideload.ko
```

from a clean checkout, in a container (`Containerfile`, with Apple's
`container`, docker or podman): it fetches the `linux` submodule, Chumby's GPL
2.6.16 source, at the pinned commit, and Chumby's toolchain (gcc 4.3), pinned
by its hash; configures the kernel as the stock one, with `patches/` for a
newer make; and runs `module/build.sh`, which sets the module's vermagic to the
stock kernel's, `2.6.16-csb ARMv5 gcc-4.3`.

## On the Chumby

`boot/userhook2` does all of this at boot, from a tree whose `sideload/` holds
`sideload.ko`, `zImage`, `initramfs.cpio.gz` and `neochumby-handoff`
(`tools/tree --sideload DIR`, `tools/stick`). From NFS, each is a copy
neochumby-boot checked against the manifest neochumbyd signed; without
`/psp/neochumby-boot` it doesn't sideload from NFS at all. Stock's own software
goes first, as it holds the memory the blocks need:

```sh
/usr/chumby/scripts/stop_control_panel
killall chumbhowld btplayd chumbalarmd crond
sync; echo 3 > /proc/sys/vm/drop_caches
insmod sideload.ko
cat zImage > /proc/sideload-kernel
neochumby-handoff initramfs.cpio.gz /psp > /proc/sideload-initrd
echo 1 > /proc/sideload-go
```

Any failure before the jump unloads the module and returns to the stock boot.

## Safety

- The module writes the four reserved RAM blocks (one unused), the ATAGS/stub
  region and the two watchdog service registers.  It does not touch
  `/dev/mtd`, the NAND controller or `/psp`.
- The target kernel has `CONFIG_MTD=n` and no NAND driver.
- The stock NAND and the stock kernel are unchanged; a reboot runs them.

## Tested

A real Chumby Classic, from its stock 2.6.16: Linux 7 boots, with the session,
Wi-Fi from `/psp/network_config`, and touch. `dev/emulate-stock` runs the boot
hook's way there on the emulated Chumby, from your own firmware.

Evidence: `evidence/7.3.1-qemu.log` and `evidence/7.3.1-panel.jpg`, from the
first boot of 7.3.1, whose panel was still badly artifacted.

## Layout

| path | what |
| --- | --- |
| `module/sideload_module.c` | the 2.6.16 module: four blocks, `/proc`, ATAGS, watchdog, jump |
| `module/stub.S` | the position-independent MMU-off jump |
| `module/build.sh` | builds the module and sets its stock vermagic |
| `build`, `Containerfile`, `patches/` | the module's build from a clean checkout |
| `linux` | Chumby's GPL 2.6.16 source, a submodule (`update = none`) |
| `evidence/7.3.1-*` | the first 7.3.1 QEMU log and the hardware panel photo |

## Limits

- The stock 2.6.16 must give root to load the module: the boot hook has it.
- The stock allocator gives 4 MiB per block: the zImage and the initramfs, with
  `/psp`, must each fit one, and the Image must end below the zImage at 12 MiB.
- The jump is one-way until a power cycle.
