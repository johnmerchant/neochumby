//! The Chumby Classic's inputs: the resistive touchscreen (TSC2100, evdev on
//! /dev/input/event0, or event1 on the dist kernels: [`touchscreen`]), the
//! squeeze switch (/dev/switch, one byte; on the dist kernels an input
//! device: [`open_button`]) and the accelerometer
//! (/dev/accel, 56-byte records). Neither Chumby driver supports poll(), so
//! everything is read non-blocking, each tick.
//!
//! A touch can also be made up: evdev takes input_event records written to
//! the touchscreen's device and hands them to every reader as if the
//! touchscreen had sent them ([`touch_down`], [`touch_up`], at
//! [`Calibration::raw`]).
//! The developer link does this for `neochumbyd tap`.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::RawFd;
use std::os::unix::fs::OpenOptionsExt;

use neochumby_sys::fd;

use crate::fb;

/// The touchscreen's input device on the stock kernel, and where none is
/// found ([`touchscreen`]).
pub const TOUCHSCREEN: &str = "/dev/input/event0";
/// The variable naming the touchscreen's device, which the session sets
/// for an app it gives the touchscreen to: its jail has no /proc to look.
pub const TOUCHSCREEN_ENV: &str = "NEOCHUMBY_TOUCHSCREEN";

/// The touchscreen's input device: [`TOUCHSCREEN_ENV`]'s, else the first
/// in /proc/bus/input/devices that reports an absolute X and Y, else
/// [`TOUCHSCREEN`]. The stock kernel has it as event0; the dist kernels
/// list the top button first, and have it as event1.
pub fn touchscreen() -> String {
    if let Ok(p) = std::env::var(TOUCHSCREEN_ENV)
        && p.starts_with("/dev/input/event")
    {
        return p;
    }
    let list = std::fs::read_to_string("/proc/bus/input/devices").unwrap_or_default();
    find_touchscreen(&list).unwrap_or_else(|| TOUCHSCREEN.into())
}

/// In /proc/bus/input/devices' blocks, the event device of the first that
/// has ABS_X and ABS_Y: its `B: ABS=` mask, hex words with the lowest last.
fn find_touchscreen(list: &str) -> Option<String> {
    list.split("\n\n").find_map(|block| {
        let line = |prefix: &str| block.lines().find_map(|l| l.strip_prefix(prefix));
        let abs = line("B: ABS=")?.split_whitespace().last()?;
        let xy = u64::from_str_radix(abs, 16).ok()? & 3 == 3;
        let event = line("H: Handlers=")?.split_whitespace().find(|h| h.starts_with("event"))?;
        xy.then(|| format!("/dev/input/{event}"))
    })
}
/// The top button, opened non-blocking: the stock kernel's /dev/switch,
/// else, on the dist kernels, which have none, the input device they make
/// for it (gpio-keys-polled's "chumby-button": [`find_button`]). Its path
/// with it. Either is read with [`button_level`].
pub fn open_button() -> std::io::Result<(File, String)> {
    let open = |p: &str| OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(p);
    match open(SWITCH) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let list = std::fs::read_to_string("/proc/bus/input/devices").unwrap_or_default();
            let dev = find_button(&list).ok_or(e)?;
            Ok((open(&dev)?, dev))
        }
        r => r.map(|f| (f, SWITCH.to_string())),
    }
}

/// The stock kernel's top button: one byte, its level.
pub const SWITCH: &str = "/dev/switch";

/// In /proc/bus/input/devices' blocks, the event device of the dist
/// kernels' top button: keys, no absolute axes, and named for it (the board
/// file's "chumby-button", or gpio-keys').
fn find_button(list: &str) -> Option<String> {
    list.split("\n\n").find_map(|block| {
        let line = |prefix: &str| block.lines().find_map(|l| l.strip_prefix(prefix));
        let name = line("N: Name=")?.trim_matches('"');
        let named = name == "chumby-button" || name.starts_with("gpio-keys");
        let keys = line("B: KEY=").is_some() && line("B: ABS=").is_none();
        let event = line("H: Handlers=")?.split_whitespace().find(|h| h.starts_with("event"))?;
        (named && keys).then(|| format!("/dev/input/{event}"))
    })
}

/// The top button's level now (true while pressed), from its open file of
/// either kind ([`open_button`]): an input device is asked which keys are
/// down; /dev/switch reads as one byte. None if it can't be read.
pub fn button_level(f: &mut File) -> Option<bool> {
    if is_input_device(f) {
        return neochumby_sys::evdev::any_key_down(f).ok();
    }
    let mut b = [0u8; 1];
    match f.read(&mut b) {
        Ok(1) => Some(b[0] != 0),
        _ => None,
    }
}

/// Whether `f` is an input device (character major 13), as the dist
/// kernels' button is and /dev/switch isn't.
fn is_input_device(f: &File) -> bool {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    f.metadata().is_ok_and(|m| {
        let d = m.rdev();
        m.file_type().is_char_device() && (((d >> 8) & 0xfff) | ((d >> 32) & !0xfff)) == 13
    })
}

/// The stock firmware's touchscreen calibration ([`Calibration`]).
pub const CALIBRATION: &str = "/psp/ts_settings";

/// The variable holding the top button's descriptor, which the session
/// hands to an app whose manifest says `button = true`.
pub const BUTTON_ENV: &str = "NEOCHUMBY_BUTTON";

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_PRESSURE: u16 = 0x18;
const BTN_TOUCH: u16 = 0x14a;
/// What a made-up touch reports as its pressure; anything above 0 is down.
const PRESSURE: i32 = 200;

fn open_nonblock(path: &str) -> Option<File> {
    OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path).ok()
}

/// Raw touchscreen coordinates to screen pixels, from the stock firmware's
/// own calibration in /psp/ts_settings: "x0,xspan,y0,yspan,swap" with a
/// negative span for an inverted axis.
#[derive(Clone, Copy, Debug)]
pub struct Calibration {
    x0: i32,
    xspan: i32,
    y0: i32,
    yspan: i32,
    swap: bool,
}

impl Calibration {
    pub fn load() -> Calibration {
        let fallback = Calibration { x0: 169, xspan: 3733, y0: 4082, yspan: -4054, swap: false };
        let Ok(s) = std::fs::read_to_string(CALIBRATION) else { return fallback };
        let v: Vec<i32> = s.trim().split(',').filter_map(|f| f.trim().parse().ok()).collect();
        match v[..] {
            [x0, xspan, y0, yspan, swap] if xspan != 0 && yspan != 0 => {
                Calibration { x0, xspan, y0, yspan, swap: swap != 0 }
            }
            _ => fallback,
        }
    }

    pub fn map(&self, rx: i32, ry: i32) -> (i32, i32) {
        let (rx, ry) = if self.swap { (ry, rx) } else { (rx, ry) };
        let x = (rx - self.x0) * fb::W as i32 / self.xspan;
        let y = (ry - self.y0) * fb::H as i32 / self.yspan;
        (x.clamp(0, fb::W as i32 - 1), y.clamp(0, fb::H as i32 - 1))
    }

    /// The raw reading [`Calibration::map`] takes to pixel (x, y): the
    /// pixel's middle, so that `map` (which rounds toward zero) comes back
    /// to it for any span of at least two raw steps a pixel, as every real
    /// calibration is (about twelve on the Classic).
    pub fn raw(&self, x: i32, y: i32) -> (i32, i32) {
        let a = self.x0 + (2 * x + 1) * self.xspan / (2 * fb::W as i32);
        let b = self.y0 + (2 * y + 1) * self.yspan / (2 * fb::H as i32);
        if self.swap { (b, a) } else { (a, b) }
    }
}

/// One of the 2.6.16 kernel's 32-bit input_event records: timeval (2 x
/// u32), type u16, code u16, value i32, little-endian. Written ones carry
/// no time: the kernel stamps them.
fn event(ty: u16, code: u16, value: i32) -> [u8; 16] {
    let mut e = [0u8; 16];
    e[8..10].copy_from_slice(&ty.to_le_bytes());
    e[10..12].copy_from_slice(&code.to_le_bytes());
    e[12..16].copy_from_slice(&value.to_le_bytes());
    e
}

/// A raw coordinate far from `v`, still in the 12-bit range.
fn elsewhere(v: i32) -> i32 {
    if v < 2048 { v + 2048 } else { v - 2048 }
}

/// What a finger going down at raw (rx, ry) reports, to write to
/// [`touchscreen`]. The input core passes on a coordinate only when it
/// changes (beyond the driver's fuzz), so a reader opened since the last
/// touch there would never learn where this one is: each coordinate is
/// first sent somewhere else, and readers take the last before EV_SYN.
/// Both the pressure and BTN_TOUCH, as the touchscreen's driver may report
/// either.
pub fn touch_down(rx: i32, ry: i32) -> Vec<u8> {
    [
        event(EV_ABS, ABS_X, elsewhere(rx)),
        event(EV_ABS, ABS_Y, elsewhere(ry)),
        event(EV_ABS, ABS_X, rx),
        event(EV_ABS, ABS_Y, ry),
        event(EV_ABS, ABS_PRESSURE, PRESSURE),
        event(EV_KEY, BTN_TOUCH, 1),
        event(EV_SYN, 0, 0),
    ]
    .concat()
}

/// What the finger coming up reports, to write to [`touchscreen`].
pub fn touch_up() -> Vec<u8> {
    [event(EV_ABS, ABS_PRESSURE, 0), event(EV_KEY, BTN_TOUCH, 0), event(EV_SYN, 0, 0)].concat()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Touch {
    /// Finger down or moved, in screen pixels, with the raw reading.
    Down {
        x: i32,
        y: i32,
        raw: (i32, i32),
    },
    Up,
}

pub struct Touchscreen {
    file: Option<File>,
    cal: Calibration,
    /// The last raw X and Y. evdev sends a coordinate only when it
    /// changes, so a reader opened since would never hear one a new touch
    /// shares with the last (a tap straight below the one that opened the
    /// app): they start as the device holds them (EVIOCGABS), and are None
    /// only where it can't say.
    raw: (Option<i32>, Option<i32>),
    pressed: bool,
    was_pressed: bool,
    /// A touch was down as the reader opened (the corner hold, or the tap
    /// that opened the app, still held): it is someone else's, and nothing
    /// is reported until it has lifted.
    held: bool,
}

/// What a touchscreen holds as a reader opens it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Now {
    raw: (Option<i32>, Option<i32>),
    pressed: bool,
}

impl Now {
    /// What `file` holds now: X, Y, and whether the pressure is above 0.
    fn of(file: &File) -> Now {
        let axis = |a: u16| neochumby_sys::evdev::abs_value(file, a).ok();
        Now { raw: (axis(ABS_X), axis(ABS_Y)), pressed: axis(ABS_PRESSURE).is_some_and(|p| p > 0) }
    }
}

impl Touchscreen {
    pub fn open() -> Touchscreen {
        let file = open_nonblock(&touchscreen());
        let now = file.as_ref().map(Now::of).unwrap_or_default();
        Touchscreen::with(file, Calibration::load(), now)
    }

    fn with(file: Option<File>, cal: Calibration, now: Now) -> Touchscreen {
        Touchscreen { file, cal, raw: now.raw, pressed: now.pressed, was_pressed: false, held: now.pressed }
    }

    /// Every touch report since the last call, in order.
    pub fn poll(&mut self) -> Vec<Touch> {
        let mut out = Vec::new();
        let mut buf = [0u8; 16 * 32];
        while let Some(Ok(n)) = self.file.as_mut().map(|f| f.read(&mut buf)) {
            if n == 0 {
                break;
            }
            self.take(&buf[..n], &mut out);
        }
        out
    }

    /// The touches in `records`, input_events as [`event`] lays them out.
    fn take(&mut self, records: &[u8], out: &mut Vec<Touch>) {
        for e in records.as_chunks::<16>().0 {
            let ty = u16::from_le_bytes([e[8], e[9]]);
            let code = u16::from_le_bytes([e[10], e[11]]);
            let value = i32::from_le_bytes([e[12], e[13], e[14], e[15]]);
            match (ty, code) {
                (EV_ABS, ABS_X) => self.raw.0 = Some(value),
                (EV_ABS, ABS_Y) => self.raw.1 = Some(value),
                (EV_ABS, ABS_PRESSURE) => self.pressed = value > 0,
                (EV_KEY, BTN_TOUCH) => self.pressed = value != 0,
                (EV_SYN, _) if self.held => self.held = self.pressed,
                (EV_SYN, _) => {
                    if let (true, (Some(rx), Some(ry))) = (self.pressed, self.raw) {
                        let (x, y) = self.cal.map(rx, ry);
                        out.push(Touch::Down { x, y, raw: (rx, ry) });
                        self.was_pressed = true;
                    } else if !self.pressed && self.was_pressed {
                        out.push(Touch::Up);
                        self.was_pressed = false;
                    }
                }
                _ => {}
            }
        }
    }
}

/// The squeeze sensor in the case: true while squeezed.
pub struct Squeeze(Option<File>);

impl Squeeze {
    /// The switch the session handed over ([`BUTTON_ENV`]), else
    /// /dev/switch. Only one process at a time can have /dev/switch open,
    /// and the session keeps it, so an app that wants it says `button =
    /// true` in its manifest.
    pub fn open() -> Squeeze {
        let var = std::env::var(BUTTON_ENV).ok();
        Squeeze(handed_over(var.as_deref()).or_else(|| open_button().ok().map(|(f, _)| f)))
    }

    pub fn squeezed(&mut self) -> bool {
        self.0.as_mut().and_then(button_level).unwrap_or(false)
    }
}

/// A copy of the switch's descriptor from the session (`var`, the value of
/// [`BUTTON_ENV`]), so that opening it twice doesn't close it twice.
/// Neither is passed on to programs this one runs.
fn handed_over(var: Option<&str>) -> Option<File> {
    let n: RawFd = var?.parse().ok()?;
    fd::handed_over(n).map(File::from)
}

/// One accelerometer record: 14 little-endian u32s, the first a version (3),
/// the second a timestamp, then instantaneous x/y/z, averaged x/y/z and
/// more. About 2048 is zero; 1 g is roughly 830 counts.
pub struct Accel(Option<File>);

impl Accel {
    pub fn open() -> Accel {
        Accel(open_nonblock("/dev/accel"))
    }

    /// The averaged x, y, z, centred on zero.
    pub fn read(&mut self) -> Option<[i32; 3]> {
        let mut b = [0u8; 56];
        let n = self.0.as_mut()?.read(&mut b).ok()?;
        if n < 32 {
            return None;
        }
        let word = |i: usize| u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]) as i32;
        Some([word(5) - 2048, word(6) - 2048, word(7) - 2048])
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;

    use super::*;

    /// The stock calibration (the fallback), and others: swapped axes,
    /// neither inverted, both, and a narrow one.
    fn calibrations() -> [Calibration; 5] {
        let c = |x0, xspan, y0, yspan, swap| Calibration { x0, xspan, y0, yspan, swap };
        [
            c(169, 3733, 4082, -4054, false),
            c(4000, -3800, 150, 3900, true),
            c(100, 3900, 120, 3850, false),
            c(3990, -3870, 4010, -3900, true),
            c(1000, 640, 1000, -480, false),
        ]
    }

    #[test]
    fn pixels_go_to_raw_and_back() {
        for cal in calibrations() {
            for x in 0..fb::W as i32 {
                for y in (0..fb::H as i32).step_by(7).chain([fb::H as i32 - 1]) {
                    let (rx, ry) = cal.raw(x, y);
                    assert!((0..4096).contains(&rx) && (0..4096).contains(&ry), "{cal:?} ({x}, {y}): ({rx}, {ry})");
                    assert_eq!(cal.map(rx, ry), (x, y), "{cal:?}");
                }
            }
        }
    }

    #[test]
    fn a_made_up_touch_reads_as_a_real_one() {
        for cal in calibrations() {
            let mut t = Touchscreen::with(None, cal, Now::default());
            let mut got = Vec::new();
            let (rx, ry) = cal.raw(10, 200);
            t.take(&touch_down(rx, ry), &mut got);
            t.take(&touch_up(), &mut got);
            // Again at the same place, as a reader opened since sees it.
            let mut fresh = Touchscreen::with(None, cal, Now::default());
            fresh.take(&touch_down(rx, ry), &mut got);
            assert_eq!(
                got,
                [Touch::Down { x: 10, y: 200, raw: (rx, ry) }, Touch::Up, Touch::Down { x: 10, y: 200, raw: (rx, ry) }]
            );
            assert!(touch_down(rx, ry).len().is_multiple_of(16) && touch_up().len() == 48);
        }
        assert!(Touchscreen::with(None, calibrations()[0], Now::default()).poll().is_empty());
    }

    /// What a real touchscreen sends for a touch at the place it last
    /// reported: the pressure, and no coordinate, which hasn't changed.
    fn press_in_place() -> Vec<u8> {
        [event(EV_ABS, ABS_PRESSURE, PRESSURE), event(EV_KEY, BTN_TOUCH, 1), event(EV_SYN, 0, 0)].concat()
    }

    #[test]
    fn a_touch_where_the_last_one_was_reaches_a_reader_opened_since() {
        let cal = calibrations()[0];
        let (rx, ry) = cal.raw(160, 120);
        // The device holds the last place: a touch there again sends no
        // coordinate, and the reader knows it from when it opened.
        let mut t = Touchscreen::with(None, cal, Now { raw: (Some(rx), Some(ry)), pressed: false });
        let mut got = Vec::new();
        t.take(&press_in_place(), &mut got);
        t.take(&touch_up(), &mut got);
        assert_eq!(got, [Touch::Down { x: 160, y: 120, raw: (rx, ry) }, Touch::Up]);
        // Where the device can't say, nothing is made up.
        let mut blind = Touchscreen::with(None, cal, Now::default());
        let mut got = Vec::new();
        blind.take(&press_in_place(), &mut got);
        assert!(got.is_empty());
    }

    #[test]
    fn a_touch_held_as_the_reader_opens_is_not_its_own() {
        let cal = calibrations()[0];
        let (rx, ry) = cal.raw(5, 5);
        let mut t = Touchscreen::with(None, cal, Now { raw: (Some(rx), Some(ry)), pressed: true });
        let mut got = Vec::new();
        // Still held, then lifted: nothing.
        t.take(&press_in_place(), &mut got);
        t.take(&touch_up(), &mut got);
        assert!(got.is_empty());
        // The next touch is.
        let (tx, ty) = cal.raw(160, 120);
        t.take(&touch_down(tx, ty), &mut got);
        assert_eq!(got, [Touch::Down { x: 160, y: 120, raw: (tx, ty) }]);
    }

    #[test]
    fn the_top_button_is_the_device_named_for_it() {
        // As the dist kernels list them: the top button, then the TSC2100.
        let dist = "I: Bus=0019 Vendor=0001 Product=0001 Version=0100\nN: Name=\"chumby-button\"\n\
                    H: Handlers=kbd event0 \nB: EV=3\nB: KEY=10000000\n\n\
                    I: Bus=0000 Vendor=0000 Product=0000 Version=0000\nN: Name=\"tsc2100_ts\"\n\
                    H: Handlers=event1 \nB: EV=b\nB: KEY=400 0 0 0 0 0 0 0 0 0 0\nB: ABS=1000003\n";
        assert_eq!(find_button(dist).as_deref(), Some("/dev/input/event0"));
        let keys = "N: Name=\"gpio-keys\"\nH: Handlers=kbd event2 \nB: KEY=100000 0 0 0\n";
        assert_eq!(find_button(keys).as_deref(), Some("/dev/input/event2"));
        // The stock kernel's touchscreen only: no button among the input devices.
        assert_eq!(find_button("N: Name=\"tsc2100\"\nH: Handlers=event0 \nB: KEY=400\nB: ABS=1000003\n"), None);
        assert_eq!(find_button(""), None);
    }

    #[test]
    fn a_switch_reads_as_a_byte() {
        // Not an input device: read as /dev/switch is, its level a byte.
        let p = std::env::temp_dir().join(format!("neochumby-switch-{}", std::process::id()));
        std::fs::write(&p, [1u8]).unwrap();
        assert_eq!(button_level(&mut File::open(&p).unwrap()), Some(true));
        std::fs::write(&p, [0u8]).unwrap();
        assert_eq!(button_level(&mut File::open(&p).unwrap()), Some(false));
        std::fs::write(&p, []).unwrap();
        assert_eq!(button_level(&mut File::open(&p).unwrap()), None);
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn the_touchscreen_is_the_device_with_x_and_y() {
        // As the dist kernels list them: the top button, then the TSC2100.
        let dist = "I: Bus=0019 Vendor=0001 Product=0001 Version=0100\nN: Name=\"gpio-keys\"\n\
                    H: Handlers=kbd event0 \nB: EV=3\nB: KEY=100000 0 0 0\n\n\
                    I: Bus=0000 Vendor=0000 Product=0000 Version=0000\nN: Name=\"tsc2100_ts\"\n\
                    H: Handlers=event1 \nB: EV=b\nB: KEY=400 0 0 0 0 0 0 0 0 0 0\nB: ABS=1000003\n";
        assert_eq!(find_touchscreen(dist).as_deref(), Some("/dev/input/event1"));
        let stock = "N: Name=\"tsc2100\"\nH: Handlers=event0 \nB: ABS=1000003\n";
        assert_eq!(find_touchscreen(stock).as_deref(), Some("/dev/input/event0"));
        assert_eq!(find_touchscreen("N: Name=\"x\"\nH: Handlers=event2\nB: ABS=4\n"), None);
        assert_eq!(find_touchscreen(""), None);
    }

    #[test]
    fn squeeze_takes_the_button_it_was_handed() {
        let switch = File::open("/dev/null").unwrap();
        let var = switch.as_raw_fd().to_string();
        let mut a = Squeeze(handed_over(Some(&var)));
        let b = Squeeze(handed_over(Some(&var)));
        let fd = |s: &Squeeze| s.0.as_ref().map(|f| f.as_raw_fd());
        // Each a copy of it, not /dev/switch (which a host hasn't got).
        assert!(fd(&a).is_some() && fd(&b).is_some() && fd(&a) != fd(&b));
        assert!(!a.squeezed());
        drop(a);
        drop(b);
        // Still open: the copies were closed, not the one handed over.
        assert!(switch.metadata().is_ok());
        assert!(handed_over(Some("x")).is_none() && handed_over(None).is_none());
    }
}
