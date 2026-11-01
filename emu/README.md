# The emulator

All of neochumby's emulation happens through
[qemu-chumby](https://github.com/johnmerchant/qemu-chumby), the submodule
`qemu/qemu-chumby` here at a pinned commit: QEMU's `chumby-classic`
machine, `chumby-machine` (which runs it headless and drives it, for
scripts and tests), `chumby-image` (which makes its NAND image from your
own flash backup) and `chumby-emu` (the Chumby as a desktop app). Its
README documents them, the machine's control surface and how to build it.

    emu/qemu/fetch      # qemu-chumby's prebuilt binary, into emu/qemu/out/obj/qemu-system-arm
    emu/qemu/build      # or its build from source, into the same place

`dev/emulate` (and so `dev/e2e`) runs them itself when that binary is
missing, or was made from another qemu-chumby commit than the one pinned:
`emu/qemu/fetch`, else, saying why, `emu/qemu/build` (CONTRIBUTING.md,
"Testing without a Chumby"). Run them by hand to have it ready, or
`dev/emulate prepare`.

`emu/qemu/fetch` downloads the `qemu-system-arm` that qemu-chumby's CI
built from the commit pinned here (qemu-chumby's README, "Prebuilt
binaries"; Linux x86_64 and aarch64, macOS on Apple silicon), checks its
SHA-256 against the one committed in `emu/qemu/qemu-chumby.sha256` (never
the release's own, which whoever can write to qemu-chumby could replace)
before extracting or running it, and checks that it runs. It exits 1 when
that file has no digest for the pinned commit and this platform, the
download doesn't match, or the binary can't run here; then use
`emu/qemu/build`. CI does just that: the prebuilt binary, else a build from
source, cached.

To move the submodule to a new qemu-chumby commit: once it is on
qemu-chumby's main and its CI has published it, commit the submodule's
change, run `emu/qemu/fetch --pin` (which downloads that commit's tarballs,
computes their SHA-256 here and rewrites `emu/qemu/qemu-chumby.sha256`),
and commit the new pins with it, for review in the pull request's diff.
Without new pins, CI builds QEMU from source. The prebuilt Linux binary has no window or sound (VNC and
headless only), and needs glib, pixman, libpng and zlib, which a stock
Ubuntu 24.04 runner has; build it yourself for the desktop.

`dev/emulate` boots a neochumby tree on it in one of two profiles: `dist`
(neochumby's Linux 7; CI runs it) and `stock` (the Chumby's own firmware,
from your own backup, for the sideload's handoff; local only). CONTRIBUTING.md, "Testing without a Chumby",
says how to use it, and `docs/legal.md` what may and may not be done with
the firmware.
