# The emulator

All of neochumby's emulation happens through
[qemu-chumby](https://github.com/johnmerchant/qemu-chumby), the submodule
`qemu/qemu-chumby` here at a pinned commit: QEMU's `chumby-classic`
machine, `chumby-machine` (which runs it headless and drives it, for
scripts and tests), `chumby-image` (which makes its NAND image from your
own flash backup) and `chumby-emu` (the Chumby as a desktop app). Its
README documents them, the machine's control surface and how to build it.

    emu/qemu/build      # qemu-chumby's build, into emu/qemu/out/obj/qemu-system-arm
    emu/qemu/fetch      # or its prebuilt binary, into the same place

`emu/qemu/fetch` downloads the `qemu-system-arm` that qemu-chumby's CI
built from the commit pinned here (qemu-chumby's README, "Prebuilt
binaries"; Linux x86_64 and aarch64, macOS on Apple silicon), checks its
SHA-256 and that it runs, and exits 1 when there is none for that commit
(one not yet on qemu-chumby's main) or it can't run here; then use
`emu/qemu/build`. CI does just that: the prebuilt binary, else a build from
source, cached. The prebuilt Linux binary has no window or sound (VNC and
headless only), and needs glib, pixman, libpng and zlib, which a stock
Ubuntu 24.04 runner has; build it yourself for the desktop.

`dev/emulate` boots a neochumby tree on it in one of three profiles:
`stock` (the Chumby's own firmware, from your own backup; local only),
`dist` (neochumby's Linux 5.9.16; CI runs it) and `classic` (Linux 2.6.16
from Chumby's GPL source). CONTRIBUTING.md, "Testing without a Chumby",
says how to use it, and `docs/legal.md` what may and may not be done with
the firmware.
