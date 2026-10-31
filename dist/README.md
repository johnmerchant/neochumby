# dist: neochumby's sideload userspace on mainline Linux

A [Buildroot](https://buildroot.org) external tree (`BR2_EXTERNAL`) for a
Chumby Classic on Linux 5.9.16, the last release with the i.MX21's board
files. userhook2 kexecs into it from the stock 2.6.16, so nothing on the
Chumby changes: a reboot goes back to the stock firmware, which kexecs
again.

**sideload.**  The kernel has no NAND driver built in.  The NAND drivers are
modules in the initramfs, and nothing loads them but root (`modprobe
mxc_nand`): `/init` never does, flashed or sideloaded, whatever device asks.
So `/proc/mtd` and `/dev/mtd*` stay absent until root asks for them.  If
someone does flash the image, it still boots, and root can load NAND for
recovery.

```
dist/build OUT        # OUT/sideload-zImage and OUT/sideload-rootfs.cpio.gz
dist/fetch OUT        # the same files, CI's build of them (below)
```

`sideload-zImage` has the initramfs built in, so it boots when flashed; it is
also the image the stock `userhook2` kexecs into.  `sideload-rootfs.cpio.gz`
is the same initramfs beside it, for userhook2 or another kexec path.  Both
carry the `sideload` label; `uname -r` reports `5.9.16-mx21-sideload`.

It runs Buildroot (2026.02.3, pinned with its hash) in a container: Apple's
container, docker or podman. The first build makes the cross toolchain
(gcc 13, static musl, ARM926EJ-S soft float) and takes a while; later ones
rebuild only what changed, and compiles go through ccache. On an x86_64
host (CI) it uses Bootlin's prebuilt armv5-eabi musl toolchain (gcc 14)
instead, which Bootlin builds only for x86_64.
`NEOCHUMBY_DIST_WORK=DIR` keeps Buildroot's work (`dl/`, `ccache/`,
`output/`) in `DIR` instead of the `neochumby-dist` volume; CI uses it to
cache the work.

### Kernel tracks: stable and mainline

```
NEOCHUMBY_DIST_KERNEL=stable dist/build OUT     # kernel.org's newest stable
NEOCHUMBY_DIST_KERNEL=mainline dist/build OUT   # kernel.org's mainline
```

build the same image on a current kernel instead of 5.9.16. Mainline
removed the i.MX21 after 5.9, and our series brings it back as a
device-tree platform; the zImage carries the Chumby's DTB appended, since
its boot loader passes ATAGs only. The toolchain is the same (headers at
5.9.16). The default stays 5.9.16 until a track has run on real hardware.

| Track | What it follows | Downloaded as |
| --- | --- | --- |
| `5.9.16` | itself: the last release with the i.MX21's board files | kernel.org's tarball |
| `stable` | the newest of kernel.org's releases with moniker `stable` (7.3.1) | kernel.org's tarball, `linux-VERSION.tar.xz` |
| `mainline` | kernel.org's `mainline`: Linus's newest tag, a release (7.3) or an -rc (7.4-rc3) | Buildroot's archive of the git tag, `linux-vVERSION-git4.tar.gz` |

Each track is pinned to one exact version, in one place:

- **the version**, in its defconfig, `configs/chumby_classic_TRACK_defconfig`
  (`BR2_LINUX_KERNEL_CUSTOM_VERSION_VALUE` for stable,
  `BR2_LINUX_KERNEL_CUSTOM_REPO_VERSION`, the tag, for mainline);
- **its hash**, in `kernel/TRACK/linux/linux.hash`, which Buildroot checks
  (and, for mainline, the tag's commit, which `dist/build` checks).
  `dist/build` refuses a hash file that doesn't name the defconfig's
  version.

Everything else of a track's is in `kernel/TRACK/`, with no version in any
path, so a bump renames nothing: `linux/` is Buildroot's patch directory
for the kernel (`BR2_GLOBAL_PATCH_DIR` names `patches` and `kernel/TRACK`;
Buildroot applies `<dir>/linux/<version>/` when there is one, else
`<dir>/linux/`, so a version-less directory applies to whatever version
is pinned), and `linux.fragment` its kernel options. 5.9.16, which never
moves, keeps `patches/linux/5.9.16/` and `board/chumby-classic/linux.fragment`.

**mainline's download.** An -rc has no kernel.org tarball with a
published checksum, so every mainline tag, release or -rc, comes the same
way: Buildroot's custom git download (`BR2_LINUX_KERNEL_CUSTOM_GIT`), which
archives the tag's tree reproducibly, so its SHA-256 can be pinned. Where
Buildroot would clone the whole history (gigabytes) to make that archive,
`dist/build` fetches only the tag (`git fetch --depth 1`), checks it is the
pinned commit, and makes the same archive with Buildroot's own helper
(`mk_tar_gz`, as Buildroot's git backend calls it: the same bytes, checked
against Buildroot's backend for v7.3), then checks the pinned hash before
Buildroot checks it again. `dist/build --archive` makes it and prints its
hash line, for a new tag.

**Keeping up: kernel-sync.** CI's `kernel-sync` job runs daily (and by
hand, `workflow_dispatch` with `kernel_sync`): `dist/kernel-sync` compares
each track's pin with kernel.org's `releases.json`, and when one has
moved, verifies the new version and commits the new pin to the branch
`kernel-sync/TRACK`, with a pull request of it. Nothing kernel.org says is
taken on trust:

- stable: the series' `sha256sums.asc`, whose signature must be the
  Kernel.org checksum autosigner's (fingerprint pinned in
  `dist/kernel-sync`), names the hash, and the tarball, downloaded, must
  have it;
- mainline: the tag's signature must be Linus Torvalds's (fingerprint
  pinned), its commit is pinned, and the hash is of `dist/build
  --archive`'s archive of it.

The pull request's CI then shows whether the patches still apply and the
image builds and boots (CI builds mainline only weekly, but always for
`kernel-sync/mainline` and for any change to `kernel/mainline/` or its
defconfig); when they don't, port them on that branch. One
pull request per track: a later bump updates the open one (replacing the
job's own commit, or adding to what someone has pushed there), and none is
ever merged but by hand. `dist/kernel-sync check` shows the pins against
kernel.org; `dist/kernel-sync bump TRACK` makes the bump in a checkout;
`dist/kernel-sync pr --dry-run` says what the job would do.

Switching kernels, or bumping a track, rebuilds the kernel and its
modules, so the initramfs never mixes them. `uname -r` reports the
kernel's own release with `-mx21-sideload`: `7.2.13-mx21-sideload`, and
for a mainline tag `7.3.0-mx21-sideload` (or `7.4.0-rc3-mx21-sideload`).

## Prebuilt: dist/fetch

```
dist/fetch [--kernel 5.9.16|stable|mainline] OUT
```

puts in `OUT` the image CI built from this checkout's inputs, in the layout
`dist/build OUT` writes, in seconds. CI publishes each kernel's build on
main (mainline's from a weekly run, below) to the rolling
[`dist-prebuilt`](https://github.com/johnmerchant/neochumby/releases/tag/dist-prebuilt)
prerelease as `neochumby-dist-INPUTS-KERNEL.tar.xz`, where `INPUTS` is the
last commit that changed what it's built from (`dist/` but this README,
`dist/fetch` and `dist/kernel-sync`, `crates/neochumby-init` and
`crates/neochumby-sys`; `dist/fetch --inputs` prints it), so any later
commit that leaves those alone finds the same build. Beside each is its corresponding source,
`-source.tar` (Buildroot's `make legal-info`: every package's source and
patches, the licenses, the manifest and the configuration).

Nothing on the release is trusted as such. Each tarball carries a GitHub
artifact attestation, which `dist/fetch` checks with `gh attestation
verify` before extracting anything: it must be signed by this repository's
`.github/workflows/ci.yml` running on `main`, on GitHub's runners. Then its
`MANIFEST` must name these inputs and this kernel (and says which Linux
version a track was, `linux:`). So it needs
[gh](https://cli.github.com), logged in (`gh auth login`), and the full
history (not a shallow clone). It exits 1 and leaves `OUT` as it was when
there's no build for these inputs (they have local changes, or changed on
a branch, or main's CI hasn't published yet) or the download doesn't
verify: build it with `dist/build` then. `classic/fetch OUT` does the same
for the classic image.

**Mainline, weekly.** Pushes and pull requests build only 5.9.16 and
stable; mainline is built at main's head every Sunday (and by hand, a
`workflow_dispatch` with `mainline`), booted and published like the
others, and with them for a change to mainline alone. So most inputs
commits have no mainline build, and `dist/fetch --kernel mainline` then
takes the newest one on the release of a commit in the checkout's
history instead, checked the same way (its `MANIFEST` naming the inputs it
was built from), with a line saying which commit that is and how far
behind. It must have the track's current pin and be at most 14 days
behind `HEAD`, so an old signed build can't stand in once newer ones are
gone; else it refuses (build it, or wait for the weekly build). With
local changes to the inputs it refuses, as for the others. Dated releases carry
5.9.16, stable and classic, not mainline.

`dev/emulate` and `dev/e2e` do this themselves, into `build/sideload-dist`
(`build/sideload-dist-stable` or `-mainline` with
`NEOCHUMBY_DIST_KERNEL=stable` or `mainline`) and
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
kernel (mainline weekly, above), or restores it whole when nothing it's
built from changed, and passes it on to the `e2e` (5.9.16, and mainline
where it's built) and `sideload` (each kernel built) jobs, and with
its legal-info to the `package` and `prebuilt` jobs, which publish it on
main (above). Buildroot's work is cached; the first run is
slow, and later runs rebuild only what changed. The `sideload` job runs
`dist/run-recovery-demo.py` with a zeroed NAND image, on changes under
`dist/` or `emu/qemu/` and on `workflow_dispatch`. The module handoff is
separate (`tools/neochumby-kernel-sideloader`), and stays local: it needs the
user's own stock firmware.

| Path | What |
| --- | --- |
| `configs/chumby_classic_defconfig` | The Buildroot configuration (5.9.16); `chumby_classic_stable_defconfig` and `_mainline_defconfig` the tracks', each with its pinned version |
| `board/chumby-classic/linux.fragment` | Our sideload kernel options on top of `imx_v4_v5_defconfig` (5.9.16; a track's is `kernel/TRACK/linux.fragment`). `dist/build` fails if any of them didn't make it into the kernel |
| `package/neochumby-init/` | `/init`: ncinit, from `crates/neochumby-init`, built by Buildroot's own Rust and linked statically. It never loads the NAND's drivers (only root does, with modprobe), loads every other module as its device appears (its modalias against depmod's `modules.alias`, for the devices there at boot too), runs a neochumby stick, keeps a shell on the serial console and, when no stick's hook has the screen, one on the screen (tty1), and reaps every orphan |
| `patches/` | 5.9.16's tarball hash, which Buildroot checks, and its patches (`linux/5.9.16/`), and the headers' hash |
| `kernel/stable/`, `kernel/mainline/` | Each track's patches and its pin's hash (`linux/`), and its kernel options (`linux.fragment`): above. Every kernel's series has Spleen 5x8 (`lib/fonts: add Spleen 5x8`), the screen console's only font: 64x30 characters on the 320x240 screen |
| `kernel-sync` | Moves the tracks' pins to kernel.org's, verified (CI's kernel-sync job) |
| `Containerfile` | The build's Linux |

What's in it so far: the kernel with the NAND recovery modules (for root), ncinit,
BusyBox, `wpa_supplicant` and the rt73's firmware. The backlight is a
backlight class device (`pwm-backlight`, levels 0-100) and the speaker's
mute the sound card's `Speaker Switch` (the machine driver's DAPM pin on
keypad column 4), which the session uses where 2.6.16 has
`/proc/sys/sense1`. Still to come: the Chumby's own devices as the session
opens them (the squeeze switch and the accelerometer, which were
out-of-tree drivers on 2.6.16), and the hook that kexecs.
