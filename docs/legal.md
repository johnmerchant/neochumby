# Firmware, trademarks and what this project distributes

This is how the project handles other people's software and names. It's
policy for contributors, not legal advice.

## Not affiliated

neochumby is an independent project. It is not affiliated with, endorsed
by or supported by Chumby Industries, Inc., Blue Octy, LLC, or anyone who
holds rights in the Chumby products, software or service. "Chumby" and
related names are trademarks of their owners, used here only to say what
hardware this software runs on.

## The Chumby's own software is never distributed

A Chumby runs its stock firmware: a Linux kernel and root disk, Adobe's
Flash Lite player, and Chumby's control panel and widgets. Parts of it are
open source (the kernel and BusyBox, under the GPL); Flash Lite and the
control panel are proprietary. This project ships none of it, in any form:

- no firmware, kernel, root disk, NAND dump or settings partition, in the
  repository, a release, a CI artifact or cache, or an issue or PR
  attachment;
- no screenshots or captures of the stock artwork (the control panel, the
  Chumby logo and octopus, widgets), including as test reference images.
  Tests that look at the stock screen check coarse properties (colours,
  proportions), not pinned pictures;
- no code copied from the stock software.

neochumby itself leaves the stock firmware as it is: it never writes to the
device's flash, and a reboot without the stick runs the stock software.

## The emulator

All emulation happens through qemu-chumby (`emu/qemu/qemu-chumby`), which
models the Chumby Classic's hardware so that unmodified software runs on
it: interoperability, not a copy of anything Chumby wrote. Its stock
profile (`dev/emulate --profile stock`) boots the stock firmware from
images you build from **your own device's** flash backup, with a fresh,
factory-like settings partition (qemu-chumby's README, "Images"). Those
images stay on your machine. Tests that need them read their paths from
the environment and are skipped without them, so CI never needs or holds
them: CI runs the scenarios on the dist profile, neochumby's own Linux
and userspace, which holds nothing of Chumby's.

The emulated crypto processor, which on a real Chumby holds its identity
for the Chumby service, has a fixed, made-up identity, and it refuses key
and challenge requests. Keep it that way: the emulator must never
impersonate a real device or stand in for one to the Chumby service.

## Game data

None is included, and none may be redistributed; see the README.

## If this ever needs to change

Shipping a ready-made image (for example, a factory-style NAND image for
the emulator), stock artwork, or anything else from the stock software
needs permission from its rights holders first. Open an issue rather than
a PR.
