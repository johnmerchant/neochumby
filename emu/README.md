# chumby-emu

An emulator of the Chumby Classic that boots the Chumby's own stock
firmware (kernel, root disk and settings partition) from a flash backup,
for running neochumby without the device. A host tool in Rust with no
dependencies, built apart from the neochumby workspace.

It is a full-system emulator: an ARM926EJ-S interpreter (ARMv5TE, ARM and
Thumb, the ARMv5 MMU) and models of the i.MX21's peripherals. The stock
2.6.16 kernel runs unmodified.

## Images

Firmware never goes in git. Build a NAND image from a backup (the
`mtdN-*.nanddump` files) and a settings directory:

    cargo build --release
    target/release/chumby-emu mkpsp PSPDIR psp.jffs2
    target/release/chumby-emu mknand BACKUPDIR psp.jffs2 nand.img

`mknand` keeps every partition as dumped except the settings partition
(PSP), which it replaces with `psp.jffs2`, and the update cache, which it
erases. PSPDIR is a fresh, factory-like settings set:

| File | Contents |
| --- | --- |
| `hostname` | `chumby` |
| `timezone`, `localtime` | `UTC`, and the root disk's own UTC zone file |
| `ts_settings` | `0,4095,4095,-4095,0`: the ADC's full range, which the emulated touchscreen reports against |
| `firsttime` | `0`, so the boot keeps that calibration rather than asking for one |
| `volume`, `mute`, `pan` | `50`, `0`, `0` |

Nothing else is needed.

## Running

    head -c $((0x15ad34)) BACKUPDIR/mtd4-Kernel1.bin > zImage
    target/release/chumby-emu boot --kernel zImage --nand nand.img --screenshot screen.png

UART1, the console, is stdin and stdout. The NAND image is written as the
Chumby writes its flash (`--nand-ro` keeps it unchanged). `--screenshot`
writes the screen every emulated second. `--realtime` holds emulated time
to the wall clock; without it an idle Chumby runs ahead.

`--control 127.0.0.1:5810` takes commands over TCP, one per line, each
answered `ok` once done: `down X Y`, `up`, `tap X Y [MS]` (screen
pixels; held MS emulated milliseconds, default 100), `shot FILE` (PNG, or
PPM if FILE ends in `.ppm`) and `console TEXT` (typed on the serial
console, then Enter):

    printf 'console uname -a\nshot /tmp/screen.png\n' | nc 127.0.0.1 5810

`boot` with no options lists the rest, including diagnostics
(`--history`, `--trace-at`; `CHUMBY_LOG_IO=all` or address ranges such as
`10017000-10017fff`, and `CHUMBY_LOG_CP=1`).

## Tests

`tests/scenarios.rs` boots the emulated Chumby and checks, end to end:

| Scenario | What it checks |
| --- | --- |
| `stock_boot` | The stock firmware boots to its console shell, its Flash player runs and its UI is on the screen |
| `session_home` | The boot hook runs neochumby from a tree and the session shows neochumby-home |
| `app_and_escape` | Home opens an app (`tests/probe`, which shows a known screen and logs its touches), a tap reaches it, and holding the top-left corner ends it and brings home back |
| `chumby_mode` | CHUMBY MODE ends neochumby and the stock boot carries on to its UI |

They need your own images, named by environment variables; without them
each scenario is skipped, not failed:

| Variable | What |
| --- | --- |
| `NEOCHUMBY_EMU_KERNEL` | A zImage, as above |
| `NEOCHUMBY_EMU_NAND` | A NAND image from your own flash backup (`mknand`, above) |
| `NEOCHUMBY_EMU_PSP` | A factory-like settings directory (above) |
| `NEOCHUMBY_EMU_TREE` | A tree (`tools/tree`), for the neochumby scenarios; `tests/run` builds one with the probe app |

    tests/run                       # builds a tree, then runs every scenario
    tests/run chumby_mode           # one of them
    cargo test --release --test scenarios    # with NEOCHUMBY_EMU_TREE set

Each scenario boots its own copy of the NAND image, with its settings and
Cache partitions rewritten (`patchnand`): the settings get `disable_intro`
(the first-boot tour waits for a tap) and, with a tree,
`rfs1/userhook2` from `tests/userhook2`. The emulated Chumby has no USB
storage yet, so the tree goes in the Cache partition (24 MiB, JFFS2,
`mkjffs2`) laid out as `tools/stick` lays out a stick; the test hook mounts
it at `/mnt/usb` and runs the stick's own `userhook2`, and copies its log
to the console, which is how the scenarios follow the session. Screens are
checked by the colours of regions, never against stored pictures.

Everything a scenario did stays in `target/tmp/emutest/NAME` (or
`NEOCHUMBY_EMU_WORK`): its console log, screen dumps and images. A slow
host can stretch every wait with `NEOCHUMBY_EMU_TIMEOUT_SCALE=2`.

The harness drives the emulator through a small interface
(`tests/harness/backend.rs`): boot from a kernel and a NAND image, the
serial console as a byte stream both ways, press, release and tap, a
screen dump as PPM, and stop. chumby-emu is the backend now;
`NEOCHUMBY_EMU_BACKEND=qemu` selects the QEMU one, to be written for the
i.MX21 machine (`tests/harness/qemu.rs` says how).

## What is modelled

| Block | State |
| --- | --- |
| ARM926EJ-S: ARM, Thumb, CP15, MMU | Complete for Linux; no Jazelle, no caches (memory is always coherent) |
| AITC interrupt controller, GPT timers | Complete |
| UARTs | UART1 is the console; UART3 has the crypto processor |
| Crypto processor (CP) | Answers `cpi` as hardware 3.8 with a made-up GUID; uptime |
| Clocks (CRM) | Registers, PLLs locked at the bootloader's settings |
| NAND controller and chip | Complete: reads, programs, erases, written back to the image |
| LCD controller | Background plane and graphic window, composed for screenshots |
| CSPI and TSC2100 | Touches: conversions and PINTDAV (GPIO PE19), reported by the driver on `/dev/input/event0`; audio registers only |
| USB host | Control registers and an empty root hub only |
| GPIO, PWM | Complete enough for the drivers |
| DMA, SSI audio, keypad, watchdog | Not yet: registers read back as written |

Time is emulated: one instruction is one cycle of a 266 MHz clock, and
waiting for an interrupt skips to the next timer event, so an idle Chumby
runs faster than real time.
