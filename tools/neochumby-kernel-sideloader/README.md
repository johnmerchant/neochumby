# neochumby-kernel-sideloader: stock 2.6.16 -> Linux 5.9.16 module handoff

`neochumby-kernel-sideloader` boots a NAND-less Linux 5.9.16 on the Chumby
Classic from the stock 2.6.16.  It is a 2.6.16 kernel module, loaded by root
on the stock system: the module reserves three 4 MiB blocks in RAM, root
writes the zImage and initramfs through `/proc`, and a final write builds
the ATAGS, services the watchdog, flushes the caches and jumps with the MMU
off.

The target kernel has no MTD/NAND driver, no network, no USB, no sound and
no storage.  It is the Chumby Classic's 5.9.16 board support from
`dist/patches/linux/5.9.16`, trimmed by
`kernel/chumby-classic-sideload.config`: the panel and its console, the
watchdog, and nothing that touches NAND.  It boots to Tux and a 64x30 shell
on the 320x240 panel.

A power cycle returns to the stock firmware; the stock NAND is unchanged.

## How it works

The module takes three order-10 blocks (4 MiB each), sorts them by address,
and uses the lowest as a guard, the middle for the initramfs and the highest
for the zImage.  The stock page allocator owns them until the jump.

root writes:

- `/proc/sideload-kernel` — the zImage, up to 4 MiB;
- `/proc/sideload-initrd` — the initramfs, up to 4 MiB;
- `/proc/sideload-go` — starts the handoff.

`/proc/sideload-reserve` prints the kernel and initrd physical addresses.
The zImage decompresses below `0xc0800000`, so an initramfs there is not
overwritten.  The zImage must be under 4 MiB and the decompressed Image under
8 MiB; the Chumby build is 1.54 MiB and 4.86 MiB.

The handoff builds ATAGS at `0xc3f00000`, copies the position-independent
stub to `0xc3f80000`, writes the watchdog's timeout and service registers,
disables interrupts, flushes the ARM926 caches, and jumps with the MMU off.
The target kernel's `imx2-wdt` driver then services the watchdog.

## Build

### The target kernel

The kernel script downloads Linux 5.9.16, applies the Chumby board patches
from `dist/patches/linux/5.9.16`, adds
`chumby-classic-sideload.config`, and builds:

```sh
tools/neochumby-kernel-sideloader/kernel/build.sh
```

The output is
`tools/neochumby-kernel-sideloader/build/linux-5.9.16-chumby-zImage`.  The
script checks the 4 MiB zImage and 8 MiB Image limits.
`FRAGMENT=sideload-only.config` builds the smaller QEMU/no-display kernel
instead.

### The module

Build against a 2.6.16 tree configured like the stock kernel:

```sh
KDIR=/path/to/linux-2.6.16 tools/neochumby-kernel-sideloader/module/build.sh
```

The script sets the module's vermagic to `2.6.16-csb ARMv5 gcc-4.3`, the
stock kernel's.  `CROSS_COMPILE` and `TARGET_VERMAGIC` override the defaults.
The result is `module/sideload.ko`.

### The initramfs

The module accepts any initramfs the target kernel can unpack.  `dist/build`
produces `sideload-rootfs.cpio.gz`; a minimal shell initramfs works too.  The
5.9.16 config has XZ and gzip decompressors.

## On the Chumby

root is required to load a module.  The stock SSH server, enabled by the
device's owner, or the stock boot hook on a stick, gives that root shell.
With the files in `/tmp`:

```sh
insmod /tmp/sideload.ko
cat /tmp/linux-5.9.16-chumby-zImage > /proc/sideload-kernel
cat /tmp/initramfs.cpio.gz > /proc/sideload-initrd
echo 1 > /proc/sideload-go
```

The stock SSH connection drops at the jump; the 5.9.16 kernel has no
network.  The panel shows Tux and a shell on tty1.  Power-cycle to return to
stock.

The boot hook form is the same three writes, run by the stock `userhook2`
after `insmod`.

## Safety

- The module writes the three reserved RAM blocks, the ATAGS/stub region and
  the two watchdog service registers.  It does not touch `/dev/mtd`, the NAND
  controller or `/psp`.
- The target kernel has `CONFIG_MTD=n` and no NAND driver.
- The stock NAND and the stock kernel are unchanged; a reboot runs them.

## Tested

QEMU, against a stock 2.6.16 NAND image: the module loads, the 5.9.16 Chumby
Classic kernel boots, the panel console switches to 64x30, `/proc/fb` shows
`0 IMX` and `1 IMX`, and the shell answers:

```text
Linux (none) 5.9.16-mx21-sideload #1 PREEMPT ... armv5tejl GNU/Linux
```

A real Chumby Classic, from its stock 2.6.16: the same image boots to Tux and
a `~ #` prompt on the panel.  The screen stays up and the watchdog is
serviced.

## Layout

| path | what |
| --- | --- |
| `module/sideload_module.c` | the 2.6.16 module: reserve, `/proc`, ATAGS, watchdog, jump |
| `module/stub.S` | the position-independent MMU-off jump |
| `module/build.sh` | builds the module and sets its stock vermagic |
| `kernel/build.sh` | the 5.9.16 target kernel |
| `kernel/chumby-classic-sideload.config` | the panel console, watchdog, no NAND/network/USB |
| `kernel/sideload-only.config` | the smaller QEMU/no-display config |
| `kernel/0001...patch`, `kernel/0002...patch` | the minimal kernel's NAND removal and macOS host build fix |
| `dist/run-recovery-demo.py` | tests the dist image's flashed recovery path (not this handoff) |

## Limits

- The stock 2.6.16 must give root to load the module.
- The target kernel is 5.9.16: i.MX21 was removed in 5.10.
- The target kernel has no network, USB, sound or storage.  It is a display
  console and a shell; the full neochumby session is the dist image's job.
- The jump is one-way until a power cycle.
