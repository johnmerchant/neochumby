# neochumby-kernel-sideloader: stock 2.6.16 -> Linux 5.9.16 or 7.3.1 module handoff

`neochumby-kernel-sideloader` boots a NAND-less Linux on the Chumby Classic
from the stock 2.6.16.  It is a 2.6.16 kernel module, loaded by root on the
stock system: the module reserves four 4 MiB blocks in RAM, root writes the
zImage and initramfs through `/proc`, and a final write builds the ATAGS,
services the watchdog, flushes the caches and jumps with the MMU off.

Two targets are built here:

- **5.9.16** — the Chumby Classic board support from
  `dist/patches/linux/5.9.16`, trimmed to the panel console and watchdog.
  This is the proven display path: Tux and a 64x30 shell on the panel.
- **7.3.1** — the dist stable track's device-tree i.MX21 platform, with the
  panel console and watchdog built in and USB, WiFi, sound, touch, NFS and
  VFAT as modules in the initramfs.  It boots; the panel still has artifacts
  (see "7.3.1 handoff" below).

Neither target has an MTD/NAND driver (`CONFIG_MTD=n`).  A power cycle
returns to the stock firmware; the stock NAND is unchanged.

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

| target | zImage | decompressed Image | initramfs |
| --- | --- | --- | --- |
| 5.9.16 display | 1.54 MiB | 4.86 MiB | any |
| 7.3.1 full | 2.87 MiB | 8.93 MiB | 3.87 MiB gzip |

The handoff builds ATAGS at `0xc3f00000`, copies the position-independent
stub to `0xc3f80000`, writes the watchdog's timeout and service registers,
disables interrupts, flushes the ARM926 caches, and jumps with the MMU off.
The target kernel's `imx2-wdt` driver then services the watchdog.

## Build

### 5.9.16

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

### 7.3.1

`kernel/build-7.sh` downloads Linux 7.3.1, applies the stable track's
i.MX21/Chumby patches from `dist/kernel/stable/linux`, adds the stable
fragment and `chumby-classic-7-full.config`, builds the zImage and appends
the Chumby DTB, builds the modules, and (with `ROOTFS`) packs them into the
initramfs:

```sh
tools/neochumby-kernel-sideloader/kernel/build-7.sh
# optional: ROOTFS=/path/to/sideload-rootfs.cpio.gz
```

The output is `build/linux-7.3.1-chumby-zImage` and
`build/linux-7.3.1-chumby-initramfs.cpio.gz`.  The script checks
zImage+DTB < 4 MiB, Image < 12 MiB (the four-block layout), and initramfs
< 4 MiB.

The kernel keeps the core, panel console and watchdog built in.  The
initramfs modules are USB host/storage/HID, `rt73usb` WiFi, sound/TSC2100,
touch, SPI, IIO/KXP74, NFS and VFAT.  The full config is
`kernel/chumby-classic-7-full.config`.

### The module

Build against a 2.6.16 tree configured like the stock kernel:

```sh
KDIR=/path/to/linux-2.6.16 tools/neochumby-kernel-sideloader/module/build.sh
```

The script sets the module's vermagic to `2.6.16-csb ARMv5 gcc-4.3`, the
stock kernel's.  `CROSS_COMPILE` and `TARGET_VERMAGIC` override the defaults.
The result is `module/sideload.ko`.

### The initramfs

The module accepts any initramfs the target kernel can unpack.  The 7.3.1
build takes a dist `sideload-rootfs.cpio.gz` or a minimal shell initramfs and
adds the 7.3.1 modules to it.  The current 7.3.1 image was built from the
minimal shell initramfs; the modules are present under
`/lib/modules/7.3.1-mx21-sideload` but `/init` does not load them yet.

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

For the larger 7.3.1 pair, stream them instead of filling `/tmp`:

```sh
insmod /tmp/sideload.ko
cat linux-7.3.1-chumby-zImage | ssh root@chumby 'cat > /proc/sideload-kernel'
cat linux-7.3.1-chumby-initramfs.cpio.gz | ssh root@chumby 'cat > /proc/sideload-initrd'
ssh root@chumby 'echo 1 > /proc/sideload-go'
```

The stock SSH connection drops at the jump; neither target kernel has
network.  Power-cycle to return to stock.  The boot hook form is the same
writes, run by the stock `userhook2` after `insmod`.

## Safety

- The module writes the four reserved RAM blocks (one unused), the ATAGS/stub
  region and the two watchdog service registers.  It does not touch
  `/dev/mtd`, the NAND controller or `/psp`.
- Both target kernels have `CONFIG_MTD=n` and no NAND driver.
- The stock NAND and the stock kernel are unchanged; a reboot runs them.

## Tested

5.9.16, QEMU against a stock 2.6.16 NAND image: the module loads, the 5.9.16
Chumby Classic kernel boots, the panel console switches to 64x30, `/proc/fb`
shows `0 IMX` and `1 IMX`, and the shell answers:

```text
Linux (none) 5.9.16-mx21-sideload #1 PREEMPT ... armv5tejl GNU/Linux
```

A real Chumby Classic, from its stock 2.6.16: the same image boots to Tux and
a `~ #` prompt on the panel.  The screen stays up and the watchdog is
serviced.  (The hardware run used the earlier three-block module; the
four-block module keeps the zImage and initrd at blocks 2 and 3.)

7.3.1, QEMU against the stock 2.6.16: the 7.3.1 image boots, the panel console
switches to 64x30, `/proc/fb` is `0 IMX`, `/proc/mtd` is absent, and the shell
answers:

```text
Linux (none) 7.3.1-mx21-sideload #1 PREEMPT ... armv5tejl GNU/Linux
```

A real Chumby Classic, from its stock 2.6.16: the 7.3.1 kernel boots; the
panel shows Tux and the console with severe purple/blue noise and vertical
stripes.  The kernel is up; the panel output is the remaining display issue.

Evidence: `evidence/7.3.1-qemu.log` and `evidence/7.3.1-panel.jpg`.

## 7.3.1 handoff

Current state:

- The 7.3.1 feature kernel and initramfs fit the four-block module
  (2.87 MiB / 8.93 MiB / 3.87 MiB).
- QEMU boots it cleanly to the panel console and shell.
- The real Chumby boots it, but the panel output is badly artifacted (Tux is
  visible through the noise; the console is unreadable).
- The initramfs is the minimal shell; the feature modules are packed but not
  auto-loaded.
- The 7.x `imxfb`/device-tree panel path differs from the 5.9 Chumby board:
  the 5.9 `chumby_fb_pdata` timings and the `FB_IMX_GRAPHIC_WINDOW` work are
  not in the 7.x stable patch series.  `CONFIG_FB_IMX_GRAPHIC_WINDOW` is
  dropped by 7.x Kconfig; the 7.x panel/display node is the place to fix it.

Next:

1. Fix the 7.x panel artifacts: compare the 5.9 `chumby_fb_pdata` timings
   (`320x240`, PCR/PWMR/LSCR1/DMACR, 16 bpp) and the graphic-window handling
   with `arch/arm/boot/dts/nxp/imx/imx21-chumby-classic.dts` and the 7.x
   `imxfb` driver.  Check pixel format/byte order and the LCD DMA/alpha path.
2. Add the module autoload to the initramfs `/init`: `usb-storage`, `usbhid`,
   `evdev`, `rt73usb`, `spi-imx`, `mfd_tsc2100`, `tsc2100_ts`,
   `snd-soc-imx-tsc2100`, `gpio_keys_polled`, `kxp74`, `nfs`, `vfat`.
3. Verify WiFi, USB keyboard/storage, sound and touch on the panel.
4. Re-test the 5.9 path with the four-block module.

## Layout

| path | what |
| --- | --- |
| `module/sideload_module.c` | the 2.6.16 module: four blocks, `/proc`, ATAGS, watchdog, jump |
| `module/stub.S` | the position-independent MMU-off jump |
| `module/build.sh` | builds the module and sets its stock vermagic |
| `kernel/build.sh` | the 5.9.16 target kernel |
| `kernel/chumby-classic-sideload.config` | the 5.9 panel console, watchdog, no NAND/network/USB |
| `kernel/build-7.sh` | the 7.3.1 target kernel, modules and initramfs |
| `kernel/chumby-classic-7-full.config` | the 7.3.1 panel, watchdog and feature modules |
| `kernel/sideload-only.config` | the smaller 5.9 QEMU/no-display config |
| `kernel/0001...patch`, `kernel/0002...patch` | the minimal 5.9 kernel's NAND removal and macOS host build fix |
| `dist/run-recovery-demo.py` | tests the dist image's flashed recovery path (not this handoff) |
| `evidence/7.3.1-*` | the 7.3.1 QEMU log and the hardware panel photo |

## Limits

- The stock 2.6.16 must give root to load the module.
- The stock allocator gives 4 MiB per block; the four-block layout allows the
  7.3.1 Image up to the zImage at 12 MiB.
- The 7.3.1 panel artifacts and the initramfs module autoload are open.
- The 7.3.1 kernel keeps the core/display/watchdog built in; the feature
  drivers are modules, so the initramfs is close to its 4 MiB limit
  (3.87 MiB).
- The jump is one-way until a power cycle.
