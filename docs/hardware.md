# The Chumby Classic: measured notes and traps

What was found on a Chumby Classic (firmware 1.7.2, Linux 2.6.16, BusyBox
1.6.1) while building a replacement UI for it. Everything here was measured
or seen on a real device; where something wasn't investigated, it says so.

## Screen

- `/proc/fb` lists **two** framebuffers, both `IMX`: `fb0`, the screen, and
  `fb1`, the i.MX21 LCD controller's hardware "graphic window" overlay. Each
  is 320x240 RGB565, 153,600 bytes. Neither pans (`ypanstep=0`, virtual size
  = visible), so there is no page flipping through fbdev.
- **The overlay is set from `/proc/driver/imxfb/`**: `enable`, `alpha` (global,
  0x00-0xFF), `key` (colour key, RGB888) and `key_en`. The stock firmware
  leaves it at `enable=1 alpha=0x80 key=0x000000 key_en=1`, so `fb1` is
  blended at 50% over everything with black transparent. It's all black,
  so invisible. Root can write these files even though they show
  `r--r--r--`. **Trap:** anything non-black left in `fb1` ghosts over the
  screen at 50%. Clear it, or disable the overlay, at startup if that ever
  shows up. Untested idea: draw popups into `fb1` and let the LCD controller
  composite them for nothing.
- From the stock kernel's source (`drivers/video/imxfb.c`): writes take a
  number in any base (`0x` hex, decimal, leading-0 octal) and ignore what
  follows it. `key` is 0xRRGGBB but keeps only six bits per channel (it
  reads back `0x%02x%02x%02x` of those, so 0xffffff reads 0x3f3f3f), and it
  matches the top six bits of each channel of the `fb1` pixel. No write
  touches `fb1`'s pixels or position, and none waits for vsync. **Trap:**
  setting `fb0`'s mode (`FBIOPUT_VSCREENINFO`) writes the overlay's control
  register to 0: off, alpha 0, no key.
- The sideload kernel (Linux 5.9.16, `dist/`) has the same `fb1` and
  `/proc/driver/imxfb/` files, which read and write the same way, with the
  same boot defaults (`CONFIG_FB_IMX_GRAPHIC_WINDOW`). Setting `fb0`'s mode
  there leaves the overlay alone.
- **The LCD controller's registers can't be reached from user space.** Mapping
  them through `/dev/mem` (0x10021000) succeeds, but the first read is a bus
  error: the i.MX21's peripheral bridge restricts them to supervisor mode.
  Page flipping or vsync would need a kernel module.
- **Never change the screen mode.** Switching to 8-bit with
  `FBIOPUT_VSCREENINFO` and back left the LCD controller scanning out a
  stretched, wrong-coloured picture until a reboot, though `/dev/fb0` held
  the right pixels. `FBIOPUTCMAP` is also very slow (2.5-6 s), because the
  driver logs every palette entry. Convert to RGB565 in software instead.
- Measured copy rates (one process, everything else idle):

  | | |
  | --- | --- |
  | Full-screen copy, RAM to `/dev/fb0` | 242 per second (~37 MB/s) |
  | Full-screen 50% blend | 156 per second |
  | 320x24 strip | ~2,950 per second |
  | 64x64 colour-keyed sprite | ~4,480 per second |

  A UI that copies only changed rectangles idles at ~1% CPU. A full
  recompose at 25 fps (a scrolling panorama with layers) costs ~35%.

## Inputs

- **Touch:** `/dev/input/event0`, raw 12-bit, calibrated by `/psp/ts_settings`.
  Linux evdev accepts writes to it and delivers them to readers like a real
  touch, so taps can be scripted on the device (`dev/taps.py` does this).
  A tap can arrive with an echo shortly after the release: ignore
  presses closer than ~350 ms if your UI treats taps as buttons. The other
  way round, a steady hold reports brief releases: ignore releases shorter
  than ~400 ms when timing a hold (as the escape hatch does).
- **`/dev/switch`** (the squeeze switch / top button): one byte, `01`
  pressed, `00` released. It is **level-triggered**: every non-blocking read
  returns the current state and never `EAGAIN`, so a `while read() == 1`
  loop spins forever (seen as 84% CPU and a frozen screen). Read it once per
  frame and act on the 0 → 1 edge. While the stock Flash UI runs it is
  **opened exclusively** ("Resource busy").
- **Backlight:** `/proc/sys/sense1/brightness`, 0-65535. 9000 makes a
  pleasant night level. The `dimlevel` file next to it does nothing visible.

## Storage

- `/` is read-only and full. `/psp` is 2 MB of writable flash (settings).
  `/tmp` is a 32 MB tmpfs, so it's RAM, and gone at reboot.
- **USB stick reads are slow:** ~870 KB/s, ~175 ms per full screen, cold
  file opens up to ~370 ms, and with readahead off an 8 KB read still took
  60 ms. None of that fits in a 40 ms frame. Read from another process or
  thread, not the drawing loop.
- **Don't drop the page cache to verify writes.** `echo 3 >
  /proc/sys/vm/drop_caches` also evicts the code pages of programs running
  from the stick, which then stall (~0.6 s) while they're read back in.
- **Use a small, old, low-power stick.** A 32 GB stick plugged in at power-on
  stopped the WiFi adapter (`rt73`, on the same full-speed USB controller)
  from coming up, on three boots in a row, warm and cold. It worked when
  plugged in after boot. A 4 GB stick boots alongside the WiFi. Even good
  boots log `usb 1-2: device descriptor read/64, error -110`.
- The Chumby has no `mkdosfs` or `fdisk`. (`docfdisk` and `mkfs.jffs` are
  for its internal flash: don't.) To write a whole stick from the device,
  build a FAT image elsewhere and stream it to `/dev/sda` with `dd`.

## The stock software

- **The stock boot waits for the internet.** It sits on the boot logo until
  `www.chumby.com` answers, then fetches the control panel, and the clock
  reads 1970 until NTP answers. A Chumby on a network without internet
  access doesn't get past the logo. (Whether `userhook2` runs before that
  wait wasn't checked.)
- **The Flash watchdog.** A cron job (`flashplayer_watchdog`) restarts the
  whole control panel whenever `/tmp/flashplayer_started` exists and Flash's
  heartbeat goes quiet for 30 s. So never pause Flash with SIGSTOP (it left
  a zombie and a frozen screen). `stop_control_panel` doesn't really stop it
  either: it finds its launcher with `pidof`, which can't see shell
  scripts, so the launcher recreates the flag. None of this matters if
  nothing starts Flash, as with `userhook2`.
- **`mDNSResponder` was seen spinning at 100% CPU.** It only advertises the
  Chumby's web page. Cause not investigated; stopping it while your UI runs
  frees the CPU.
- **The clock drifted back a year** (showing 2025) after the stock control
  panel started. The Chumby's own `sync_time.sh` fixes it. Cause not
  investigated.
- Audio from the stock side: the `btplayd` daemon, driven with `btplay`
  (`playnow URL`, `stop`, `pause`, `resume`), plays plain-HTTP MP3 streams;
  ALSA's mixer control is `Master`.
- **The speaker starts muted, and may be at volume 0.** `rcS` leaves
  `/proc/sys/sense1/spkmute` at 1; the stock players unmute while they play,
  so an app that plays sound must too. The master volume restored at boot
  comes from `/psp/volume` (`chumby_set_volume`), which can be 0. Set both
  while playing and restore them afterwards, as the AirPlay app does.

## BusyBox 1.6 and the userland

- **`tar` rejects archives from macOS's bsdtar** ("corrupted octal value").
  Build strict ustar (Python's `tarfile` with `USTAR_FORMAT`).
- There's no `printf`, `od` or `head -c`. Write files with a here-doc.
- There's no `nohup` or `setsid` either. For a detached process,
  `(trap "" HUP; exec prog) &` works.
- Over SSH, `ps | grep` patterns that match a script's name also match your
  own `sh -c` command line if it mentions that name, so a `kill` built from
  them can end your own session. Put such logic in a script on the device.

## Toolchains

- **`std::thread::sleep` panics on this kernel.** An interrupted
  `nanosleep` (after a stop and continue, or a signal) fails with `EINVAL`
  on restart, and Rust's sleep asserts it can only fail with `EINTR`. Use
  `neochumby::sleep_ms`, a `poll()` with no file descriptors.
- **Atomics:** with Rust's `armv5te-unknown-linux-musleabi`, atomics go
  through compiler-builtins and the kernel's user helpers, so threads and
  atomics work (the session, AirPlay and the protocol use them on the
  device). C built with `zig cc` against zig's bundled libc is different:
  its allocator calls `__sync_*` atomics that ARMv5 has no instructions for.
  On this single-core CPU, plain non-atomic versions are correct for a
  single-threaded program.
