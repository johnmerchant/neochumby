# Evidence

Captured on 2026-10-29 with QEMU 11.1.2 (`chumby-classic`), using the
sanitized stock NAND image read-only.  The exact command is in
`qemu-command.txt`; hashes are in `sha256.txt`.

This is the NANDLESS build:

- `sideloader-demo.log` — one serial stream: stock 2.6.16 boot, the
  sideloader, the 5.9.16 boot with the built-in initramfs, the
  `NANDLESS: sideloaded` marker check, the NAND checks, and the shell.
- `run-demo.out` — driver output, including the original and scratch NAND
  SHA-256 before and after the run.
- `kernel-config.txt` — the relevant NANDLESS kernel config: the NAND
  drivers are modules, not built in.
- `qemu-command.txt` — exact QEMU command used.
- `sha256.txt` — hashes of the demonstrated artifacts.

The original NAND SHA-256 is unchanged before and after:
`5779fd4223e09f0e96eea13296f7147cf1e4baa90632368a0f5743504129db86`.
The scratch image is a separate host file; only its PSP partition differs
from the original.  The sideloaded path loads no NAND module; `/proc/mtd`,
`/dev/mtd*` and `/sys/class/mtd` are absent.  `run-demo.py` checks the
sideload marker and the module list, and fails if either NAND hash changes.
QEMU used `-snapshot` and the drive used `snapshot=on`.
