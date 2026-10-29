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

## CI

CI's `sideload` job (`.github/workflows/ci.yml`) builds the custom QEMU and
this image, then runs `run-recovery-demo.py` with a zeroed NAND image.  It
runs on changes under `dist/`, `tools/neochumby-kernel-sideloader/` or
`emu/qemu/`, and on `workflow_dispatch`, and is skipped otherwise.  QEMU and
Buildroot work is cached; the first run is slow, and later runs rebuild only
what changed.  The sideloaded path still needs the user's
own stock firmware, so it stays local (`run-demo.py`).

| Path | What |
| --- | --- |
| `configs/chumby_classic_defconfig` | The Buildroot configuration |
| `board/chumby-classic/linux.fragment` | Our sideload kernel options on top of `imx_v4_v5_defconfig`. `dist/build` fails if any of them didn't make it into the kernel |
| `board/chumby-classic/rootfs/init` | The sideload init: checks the sideload marker, loads the NAND modules only when it is absent |
| `patches/` | The kernel tarball's hash. Buildroot checks every download |
| `Containerfile` | The build's Linux |

What's in it so far: the kernel with the NAND recovery modules, BusyBox,
`kmod`, `wpa_supplicant` and the rt73's firmware. Still to come: our own init
(`neochumby-init`), the Chumby's own devices (the squeeze switch, the
accelerometer, the backlight and the speaker mute, which were out-of-tree
drivers on 2.6.16), and the hook that kexecs.
