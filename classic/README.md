# classic: the Chumby Classic's 2.6.16, from open source

An image of the Chumby Classic's own kernel, Linux 2.6.16 as Chumby
published it under the GPL, with the Chumby's drivers, and a BusyBox
initramfs: the stock firmware's kernel and devices with none of its
proprietary userspace. CI boots it on QEMU's `chumby-classic` machine to
run neochumby's static binaries on 2.6.16 without anyone's firmware image.
It is independent of `dist/` (mainline 5.9.16, the sideload image).

```
classic/build OUT    # OUT/classic-zImage, OUT/classic-rootfs.cpio.gz, OUT/classic-config
classic/boot-test QEMU OUT/classic-zImage [--stick IMG --want TEXT]
```

`classic-zImage` has the initramfs built in, so it boots on its own:

```
qemu-system-arm -M chumby-classic -kernel OUT/classic-zImage \
  -append 'console=ttyS0,38400' -nic none -serial stdio
```

`-append` is needed: without it the machine passes the stock command line,
which mounts the stock root disk from NAND. No NAND image is needed. A
shell comes up on ttyS0 (`/ # `). `classic-rootfs.cpio.gz` is the same
initramfs alone, for `-initrd`.

## What's in it

**The kernel** is the `classic/linux` submodule,
[chumby-linux-2.6.16](https://github.com/johnmerchant/chumby-linux-2.6.16)
at tag `chumby-1.7.1`: Chumby's source for firmware 1.7.1 (build 1830),
the Classic's last, exactly as published (the commit message records the
tarball's URL and SHA-256). Its configuration is the `.config` that source
carries, Chumby's own, and `uname -r` is the stock `2.6.16-csb`.
`linux.fragment` lists our only change, the built-in initramfs; the build
fails if an option of it doesn't take. `patches/linux/` has two small
patches: one lets 2.6.16's Makefile run under a current GNU make, the
other has the early boot code map the whole kernel image, which with the
initramfs built in is larger than the 4 MB 2.6.16 maps (without it, the
kernel faults before it prints anything). The submodule is `update = none`:
a plain `git submodule update` skips it, and `classic/build` fetches the
pinned commit alone when it's missing. The kernel is read from the
submodule's git objects, never its checkout, because the tree has names
that differ only by case, which macOS's disk can't hold.

**The drivers** the stock firmware loads from `/drivers`, built from the
same tree, and loaded by `/etc/init.d/rcS` as the stock boot does: the
same order and the same `version038=1` on hardware version 3.8 (from
`chumby_hwversion=` on the command line, else 3.8):

| Module | What | Devices |
| --- | --- | --- |
| `chumby-tsc2100` | TSC2100: touchscreen and audio codec | `/dev/input/event0`, `/dev/ts`, `/dev/dsp`, `/dev/mixer`, `/dev/snd/*` |
| `chumby_sense1` | button, backlight, speaker mute, headphones, watchdog | `/dev/switch`, `/proc/sys/sense1/*`, `/proc/resetReason` |
| `chumby_udma` | user DMA | `/dev/udma` |
| `chumby_timer` | aux timer | `/dev/timerx`, `/dev/timerm` |
| `chumby_accel` | accelerometer and the DC ID EEPROM | `/dev/accel`, `/dev/dcid`, `/proc/accel`, `/proc/DCversion` |

The rest is built in, as in the stock kernel: the i.MX21's serial port,
framebuffer (`/dev/fb0`), NAND (nothing mounts it) and USB host, with
usb-storage and FAT.

**The initramfs**: a static BusyBox 1.36.1 (`busybox.fragment` over its
defconfig: the applets that need more than the toolchain's glibc 2.8 are
left out), the device nodes of `initramfs.list` (2.6.16 has no devtmpfs;
the Chumby drivers' dynamic ones are made by rcS from `/proc/devices`), and
`rootfs/`. `/psp` and `/tmp` are tmpfs. Our static ARM binaries (musl,
armv5) need nothing more from it.

**A neochumby stick** is run as the stock boot runs one, by
`/etc/init.d/stick`, in the background, its output on the console: the
first USB disk's first partition (or the whole disk, if it has no
partition table) mounted read-write at `/mnt/usb`, and its `userhook2` run
with `sh`. Attach a FAT image laid out as `tools/stick` lays out a stick:

```
-drive if=none,id=stick,file=stick.img,format=raw -device usb-storage,drive=stick
```

`neochumby.hook=0` on the command line skips the stick;
`neochumby.run=PATH` runs PATH instead of `userhook2`.

## The toolchain

The stock kernel was compiled with CodeSourcery's Sourcery G++ Lite 2008q3
(gcc 4.3.2, its banner says), and so is this one: a 2.6.16 tree builds
with it unpatched, and the drivers are built as the stock ones were. The
build fetches it from Chumby's own download server, checking its SHA-256.
Its programs are 32-bit x86 Linux binaries. On an x86_64 host (CI) they
run natively with the i386 libc; elsewhere (an arm64 Mac, under Apple's
`container`) under `qemu-i386` with an i386 libc as its sysroot, which
needs the container to register a binfmt handler, so it runs with
`--cap-add ALL` (docker and podman: `--privileged`). The same compiler
builds BusyBox.

`classic/build` runs in a container (`Containerfile`): Apple's container,
docker or podman (`NEOCHUMBY_CLASSIC_RUNTIME`). Its work (the downloads,
the toolchain, the build trees) stays in a volume, `neochumby-classic`, or
in `NEOCHUMBY_CLASSIC_WORK=DIR` (CI caches it); a second run rebuilds only
what changed, and `$runtime volume rm neochumby-classic` starts over.
`NEOCHUMBY_CLASSIC_DNS=<ip>` gives the container a nameserver if the
runtime's doesn't answer.

## CI

CI's `classic` job runs on every push and uploads the image as the
`classic-image` artifact (`classic-zImage`, `classic-rootfs.cpio.gz`,
`classic-config`), for other jobs to boot. When its inputs haven't changed
it comes from a cache of the image itself; otherwise it's built (the
build's work cached too). When something under `classic/`, `emu/qemu/`
or `.gitmodules` changed, on `workflow_dispatch`, or when the image wasn't
cached, the job also runs `boot-test` twice: once bare, checking that the
kernel is the stock
release, that the modules are loaded and the built-in drivers probed, and
that the devices answer to the machine's controls over QMP (`/dev/switch`
to the button, `/proc/sys/sense1/hpin` to the headphones, `/dev/accel` to
a tilt, `/dev/input/event0` to the pen, `/dev/fb0` to the screen as QEMU
draws it, `/dev/dsp` to QEMU's audio output); and once with a stick whose
`userhook2` must run from `/mnt/usb`. The artifact is uploaded before
these tests, so a job that needs it can start as soon as it's there; a
failed test still fails the job.
