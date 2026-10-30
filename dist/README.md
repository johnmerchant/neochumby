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
dist/fetch OUT        # the same files, CI's build of them (below)
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

## Prebuilt: dist/fetch

```
dist/fetch [--kernel 5.9.16|7.2.13] OUT
```

puts in `OUT` the image CI built from this checkout's inputs, in the layout
`dist/build OUT` writes, in seconds. CI publishes each kernel's build on
main to the rolling
[`dist-prebuilt`](https://github.com/johnmerchant/neochumby/releases/tag/dist-prebuilt)
prerelease as `neochumby-dist-INPUTS-KERNEL.tar.xz`, where `INPUTS` is the
last commit that changed what it's built from (`dist/` but this README and
`dist/fetch`, `crates/neochumby-init` and `crates/neochumby-sys`;
`dist/fetch --inputs` prints it), so any later commit that leaves those
alone finds the same build. Beside each is its corresponding source,
`-source.tar` (Buildroot's `make legal-info`: every package's source and
patches, the licenses, the manifest and the configuration).

Nothing on the release is trusted as such. Each tarball carries a GitHub
artifact attestation, which `dist/fetch` checks with `gh attestation
verify` before extracting anything: it must be signed by this repository's
`.github/workflows/ci.yml` running on `main`, on GitHub's runners. Then its
`MANIFEST` must name these inputs and this kernel. So it needs
[gh](https://cli.github.com), logged in (`gh auth login`), and the full
history (not a shallow clone). It exits 1 and leaves `OUT` as it was when
there's no build for these inputs (they have local changes, or changed on
a branch, or main's CI hasn't published yet) or the download doesn't
verify: build it with `dist/build` then. `classic/fetch OUT` does the same
for the classic image.

`dev/emulate` and `dev/e2e` do this themselves, into `build/sideload-dist`
(`build/sideload-dist-7.2.13` with `NEOCHUMBY_DIST_KERNEL=7.2.13`) and
`build/classic`, when the image is missing or its inputs have changed:
`dist/fetch`, else `dist/build`, with a line saying why (CONTRIBUTING.md,
"Testing without a Chumby").

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

CI's `dist` job (`.github/workflows/ci.yml`) builds this image for each
kernel, or restores it whole when nothing it's built from changed, and
passes it on to the `e2e` (5.9.16) and `sideload` (both) jobs, and with
its legal-info to the `package` and `prebuilt` jobs, which publish it on
main (above). Buildroot's work is cached; the first run is
slow, and later runs rebuild only what changed. The `sideload` job runs
`run-recovery-demo.py` with a zeroed NAND image, on changes under `dist/`,
`tools/neochumby-kernel-sideloader/` or `emu/qemu/` and on
`workflow_dispatch`. The sideloaded path still needs the user's own stock
firmware, so it stays local (`run-demo.py`).

| Path | What |
| --- | --- |
| `configs/chumby_classic_defconfig` | The Buildroot configuration |
| `board/chumby-classic/linux.fragment` | Our sideload kernel options on top of `imx_v4_v5_defconfig`. `dist/build` fails if any of them didn't make it into the kernel |
| `package/neochumby-init/` | `/init`: ncinit, from `crates/neochumby-init`, built by Buildroot's own Rust and linked statically. It checks the sideload marker and loads the NAND modules only when it is absent (and never otherwise, whatever device asks), loads every other module as its device appears (its modalias against depmod's `modules.alias`, for the devices there at boot too), runs a neochumby stick, keeps a shell on the serial console and, when no stick's hook has the screen, one on the screen (tty1), and reaps every orphan |
| `patches/` | The kernel tarball's hash, which Buildroot checks, and our patches, per kernel version. Every kernel has Spleen 5x8 (`lib/fonts: add Spleen 5x8`), the screen console's only font: 64x30 characters on the 320x240 screen |
| `Containerfile` | The build's Linux |

What's in it so far: the kernel with the NAND recovery modules, ncinit,
BusyBox, `wpa_supplicant` and the rt73's firmware. Still to come: the
Chumby's own devices (the squeeze switch, the
accelerometer, the backlight and the speaker mute, which were out-of-tree
drivers on 2.6.16), and the hook that kexecs.
