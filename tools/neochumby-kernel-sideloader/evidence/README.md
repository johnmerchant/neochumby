# 7.3.1 handoff evidence

- `7.3.1-qemu.log` — stock 2.6.16 -> 7.3.1 full-feature image in qemu-chumby.
  The 7.3.1 kernel boots, unpacks the gzip initramfs, switches the panel
  console to 64x30, reports `0 IMX` in `/proc/fb`, has no `/proc/mtd`, and
  reaches the shell.
- `7.3.1-panel.jpg` — the real Chumby after the same handoff.  Tux and the
  console are visible through severe purple/blue noise and vertical stripes;
  the kernel is up and the panel output is the remaining display issue.

The QEMU run used the stock sanitized NAND image and the QEMU stick built by
the module demo; the hardware run streamed the zImage and initramfs into the
four-block module over SSH.  No NAND was written.
