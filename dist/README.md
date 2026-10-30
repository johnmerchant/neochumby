# dist: neochumby's sideload userspace on mainline Linux

A [Buildroot](https://buildroot.org) external tree (`BR2_EXTERNAL`) for a
Chumby Classic on Linux 5.9.16, the last release with the i.MX21's board
files. userhook2 kexecs into it from the stock 2.6.16, so nothing on the
Chumby changes: a reboot goes back to the stock firmware, which kexecs
again.

**sideload.**  The kernel has no NAND driver built in.  The NAND drivers are
modules in the initramfs, and `/init` loads them only when the kernel was
booted without the sideloader's `neochumby.sideloaded=1` marker — that is,
when the image was flashed instead of handed off.  A normal sideload never
loads them, so `/proc/mtd` and `/dev/mtd*` stay absent.  If someone does
flash the image, the built-in initramfs still boots and can load NAND for
recovery instead of leaving the Chumby unusable.

This is a deliberate trade-off: the image carries the NAND modules so a
mistaken flash is recoverable.  Root can still load them deliberately; the
normal init policy does not.

```
dist/build OUT        # OUT/sideload-zImage and OUT/sideload-rootfs.cpio.gz
```

`sideload-zImage` has the initramfs built in, so it boots when flashed and is
what the sideloader hands off.  `sideload-rootfs.cpio.gz` is the same
initramfs beside it, for userhook2 or another kexec path.  Both carry the
`sideload` label; `uname -r` reports `5.9.16-mx21-sideload`.

It runs Buildroot (2026.02.3, pinned with its hash) in a container: Apple's
container, docker or podman. The first build makes the cross toolchain
(gcc 13, static musl, ARM926EJ-S soft float) and takes a while; later ones
rebuild only what changed, and compiles go through ccache. On an x86_64
host (CI) it uses Bootlin's prebuilt armv5-eabi musl toolchain (gcc 14)
instead, which Bootlin builds only for x86_64.
`NEOCHUMBY_DIST_WORK=DIR` keeps Buildroot's work (`dl/`, `ccache/`,
`output/`) in `DIR` instead of the `neochumby-dist` volume; CI uses it to
cache the work.

### Linux 7.2.13 (optional)

```
NEOCHUMBY_DIST_KERNEL=7.2.13 dist/build OUT
```

builds the same image on Linux 7.2.13 instead
(`configs/chumby_classic_linux7_defconfig`, `board/chumby-classic/linux7.fragment`,
`patches/linux/7.2.13`). Mainline removed the i.MX21 after 5.9, and the
series brings it back as a device-tree platform; the zImage carries the
Chumby's DTB appended, since its boot loader passes ATAGs only. The
toolchain is the same (headers at 5.9.16); switching between the two
rebuilds the kernel and its modules, so the initramfs never mixes them.
`uname -r` reports `7.2.13-mx21-sideload`. The default stays 5.9.16 until
7.2.13 has run on real hardware.

## A neochumby stick

Before its shell, `/init` runs a neochumby stick as the stock boot does: it
waits up to 15 seconds for `/dev/sda1`, mounts it (FAT) on `/mnt/usb`,
and runs the stick's `userhook2` in the background, its output on the
console. That is the whole of neochumby on this image so far: its tree, on
the stick, is the session and the apps.

## Emulated, and CI

All emulation happens through qemu-chumby (`emu/qemu/qemu-chumby`), whose
`chumby-classic` machine boots this image with no firmware of Chumby's.
`dev/emulate --profile dist` boots it with a neochumby tree on a USB stick
(`-device usb-storage`), which `/init` runs as above; CI's `e2e` job runs
the end-to-end scenarios that pass there so far that way
(CONTRIBUTING.md, "Scenarios", says which, and why the others don't yet).

CI's `dist` job (`.github/workflows/ci.yml`) builds this image, or restores
it whole when nothing it's built from changed, and passes it on to the
`e2e` and `sideload` jobs. Buildroot's work is cached; the first run is
slow, and later runs rebuild only what changed. The `sideload` job runs
`run-recovery-demo.py` with a zeroed NAND image, on changes under `dist/`,
`tools/neochumby-kernel-sideloader/` or `emu/qemu/` and on
`workflow_dispatch`. The sideloaded path still needs the user's own stock
firmware, so it stays local (`run-demo.py`).

| Path | What |
| --- | --- |
| `configs/chumby_classic_defconfig` | The Buildroot configuration |
| `board/chumby-classic/linux.fragment` | Our sideload kernel options on top of `imx_v4_v5_defconfig`. `dist/build` fails if any of them didn't make it into the kernel |
| `board/chumby-classic/rootfs/init` | The sideload init: checks the sideload marker, loads the NAND modules only when it is absent, and runs a neochumby stick |
| `patches/` | The kernel tarball's hash. Buildroot checks every download |
| `Containerfile` | The build's Linux |

What's in it so far: the kernel with the NAND recovery modules, BusyBox,
`kmod`, `wpa_supplicant` and the rt73's firmware. Still to come: our own init
(`neochumby-init`), the Chumby's own devices (the squeeze switch, the
accelerometer, the backlight and the speaker mute, which were out-of-tree
drivers on 2.6.16), and the hook that kexecs.
