//! The top button (/dev/switch). Only one process at a time can have it
//! open, so the session opens it once and decides who gets each press:
//! - an app whose manifest says `button = true` (Doom and Quake fire with
//!   it) inherits the open file while it's in front, as NEOCHUMBY_BUTTON,
//!   and the session doesn't read it meanwhile;
//! - for any other app, a press ends the app in front and opens the start
//!   app with NEOCHUMBY_REASON=button, as the stock Chumby does.
//!
//! There's no hold gesture, since games hold the button to fire: the corner
//! hold is the way out of anything. On CHUMBY MODE the session closes it,
//! so the Chumby's own software can open it.
//!
//! A read returns the button's level now (1 pressed, 0 not), never "no
//! data", so the session reads it once a tick and acts on a 0 -> 1 edge.

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
    use super::Edges;

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
}
