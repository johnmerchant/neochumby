# Evidence

Captured on 2026-10-29 with QEMU 11.1.2 (`chumby-classic`), using the
sanitized stock NAND image read-only.  The exact command is in
`qemu-command.txt`; hashes are in `sha256.txt`.

These artifacts are verbatim from the build that was then named `chumpwn`.
They are kept unchanged so the captured output stays verifiable.  The tool is
now `neochumby-kernel-sideloader`; the `chumpwn` strings in the log and the
`chumpwn` hash in `sha256.txt` refer to that earlier build.

- `sideloader-demo.log` — one serial stream: stock 2.6.16 boot, the
  pre-rename loader messages, 5.9.16 boot, initramfs `/init`, interactive
  shell commands and the NAND-hidden checks.
- `run-demo.out` — driver output, including the original and scratch NAND
  SHA-256 before and after the run.
- `kernel-config.txt` — the relevant Linux 5.9.16 config options.
- `qemu-command.txt` — exact QEMU command used.
- `sha256.txt` — hashes of the demonstrated artifacts.

The original NAND SHA-256 is unchanged before and after:
`5779fd4223e09f0e96eea13296f7147cf1e4baa90632368a0f5743504129db86`.
The scratch image is a separate host file; only its PSP partition differs from
the original.  The sideloader contains no MTD device strings and opens no MTD
device; `run-demo.py` asserts this before boot.  QEMU used `-snapshot` and the
drive used `snapshot=on`.
