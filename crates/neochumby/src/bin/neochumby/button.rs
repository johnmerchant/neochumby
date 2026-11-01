//! The top button, an input device (neochumby::input::open_button). The
//! session opens it once and decides who gets each press:
//! - an app whose manifest says `button = true` (Doom and Quake fire with
//!   it) inherits the open file while it's in front, as NEOCHUMBY_BUTTON,
//!   and the session doesn't read it meanwhile;
//! - with the start app in front, a press is a `button` line on its pipe,
//!   so it can show its main screen without starting again; if the line
//!   can't be written, it's opened again as below;
//! - for any other app, a press ends the app in front and opens the start
//!   app with NEOCHUMBY_REASON=button.
//!
//! If the button can't be opened as the session starts (its module not
//! loaded yet), the session tries again every 5 s.
//!
//! On a paired Chumby whose owner allows it, neochumbyd can hold the button
//! down too (devlink.rs): the session goes by either ([`level`]), so such a
//! press goes where a real one would, except to an app with `button =
//! true`, which reads the button itself and never sees it.
//!
//! There's no hold gesture, since games hold the button to fire: the corner
//! hold is the way out of anything.
//!
//! A read gives the button's level now, so the session reads it once a tick and acts on a 0 -> 1 edge.

/// The level the session goes by: down if the button is, or neochumbyd
/// holds it down (`remote`, while the developer link is up); None with
/// neither to read.
pub fn level(real: Option<bool>, remote: Option<bool>) -> Option<bool> {
    match (real, remote) {
        (Some(a), Some(b)) => Some(a || b),
        (a, b) => a.or(b),
    }
}

/// The presses in a series of readings of the level.
#[derive(Default)]
pub struct Edges {
    last: Option<bool>,
}

impl Edges {
    /// Whether this reading is a press: up before, down now. The first
    /// reading after [`Edges::forget`] is where it starts from, so a button
    /// already held then isn't a press.
    pub fn press(&mut self, down: bool) -> bool {
        let press = self.last == Some(false) && down;
        self.last = Some(down);
        press
    }

    /// Starts again, while someone else has the button.
    pub fn forget(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{Edges, level};

    #[test]
    fn presses_are_edges() {
        let mut e = Edges::default();
        let presses: Vec<bool> = [true, true, false, true, true, false, false, true].map(|d| e.press(d)).to_vec();
        assert_eq!(presses, [false, false, false, true, false, false, false, true]);
        // Held while an app had it: not a press when the session reads again.
        e.forget();
        assert!(!e.press(true));
        assert!(!e.press(false));
        assert!(e.press(true));
    }

    #[test]
    fn a_press_from_neochumbyd_is_a_press() {
        assert_eq!(level(None, None), None);
        assert_eq!(level(Some(true), None), Some(true));
        assert_eq!(level(None, Some(true)), Some(true));
        assert_eq!(level(Some(false), Some(false)), Some(false));
        assert_eq!(level(Some(true), Some(false)), Some(true));
        assert_eq!(level(Some(false), Some(true)), Some(true));
        // Down from neochumbyd and up again: one press, as from the button.
        let mut e = Edges::default();
        let presses: Vec<bool> =
            [(false, false), (false, true), (false, true), (false, false), (true, false), (true, true)]
                .map(|(real, remote)| e.press(level(Some(real), Some(remote)).unwrap()))
                .to_vec();
        assert_eq!(presses, [false, true, false, false, true, false]);
    }
}
