# dist: neochumby's userspace on mainline Linux

A [Buildroot](https://buildroot.org) external tree (`BR2_EXTERNAL`) for a
Chumby Classic on Linux 5.9.16, the last release with the i.MX21's board
files. userhook2 kexecs into it from the stock 2.6.16, so nothing on the
Chumby changes: a reboot goes back to the stock firmware, which kexecs
again. NAND is never touched, and the kernel is built without MTD so that
it can't be.

```
dist/build OUT        # OUT/zImage and OUT/rootfs.cpio.gz
```

It runs Buildroot (2026.02.3, pinned with its hash) in a container: Apple's
container, docker or podman. The first build makes the cross toolchain
(gcc 13, static musl, ARM926EJ-S soft float) and takes a while, and later
ones rebuild only what changed.

| Path | What |
| --- | --- |
| `configs/chumby_classic_defconfig` | The Buildroot configuration |
| `board/chumby-classic/linux.fragment` | Our kernel options on top of `imx_v4_v5_defconfig`. `dist/build` fails if any of them didn't make it into the kernel |
| `patches/` | The kernel tarball's hash. Buildroot checks every download |
| `Containerfile` | The build's Linux |

What's in it so far: the kernel, BusyBox, `wpa_supplicant` and the rt73's
firmware. Still to come: our own init (`neochumby-init`), the Chumby's own
devices (the squeeze switch, the accelerometer, the backlight and the
speaker mute, which were out-of-tree drivers on 2.6.16), and the hook that
kexecs.
