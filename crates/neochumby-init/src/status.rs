//! Boot steps on the screen, as Red Hat's init scripts show them
//! (/etc/init.d/functions: echo_success and echo_failure): what's being
//! done, then at a fixed column a bracketed, coloured result, green OK, red
//! FAILED, yellow for a step skipped. The kernel's log gets none of this:
//! it keeps ncinit's plain lines.

/// How a step ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Failed,
    /// Not done, on purpose: nothing to do, or not allowed.
    Skipped,
}

impl Status {
    /// The label, six wide as Red Hat's, and its colour (SGR).
    fn label(self) -> (&'static str, &'static str) {
        match self {
            Status::Ok => ("  OK  ", "32"),
            Status::Failed => ("FAILED", "31"),
            Status::Skipped => (" SKIP ", "33"),
        }
    }
}

/// "[  OK  ]": the result's width on screen.
const WIDTH: usize = 8;

/// The line for a step `what` that ended `status`, on a terminal `cols`
/// wide: `what`, cut short to leave room, then the result at the right
/// edge (ESC[nG, a column the terminal clamps to its width), and the colour
/// back to the terminal's own (39, as Red Hat's SETCOLOR_NORMAL).
pub fn line(what: &str, status: Status, cols: usize) -> String {
    let room = cols.saturating_sub(WIDTH + 1).max(1);
    let what: String = if what.chars().count() > room {
        what.chars().take(room.saturating_sub(1)).chain(['~']).collect()
    } else {
        what.to_owned()
    };
    let (label, colour) = status.label();
    let col = cols.saturating_sub(WIDTH) + 1;
    format!("{what}\x1b[{col}G[\x1b[0;{colour}m{label}\x1b[0;39m]\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_red_hat_has_it() {
        assert_eq!(line("Loading usbhid", Status::Ok, 64), "Loading usbhid\x1b[57G[\x1b[0;32m  OK  \x1b[0;39m]\n");
        assert_eq!(line("Loading x", Status::Failed, 80), "Loading x\x1b[73G[\x1b[0;31mFAILED\x1b[0;39m]\n");
        assert!(line("Loading mtd", Status::Skipped, 64).contains("\x1b[0;33m SKIP "));
    }

    #[test]
    fn cuts_what_is_too_long() {
        let l = line(&"x".repeat(100), Status::Ok, 20);
        let what = l.split('\x1b').next().unwrap();
        assert_eq!(what.chars().count(), 11);
        assert!(what.ends_with('~'));
    }

    #[test]
    fn survives_a_tiny_terminal() {
        assert!(line("Loading usbhid", Status::Ok, 0).ends_with("]\n"));
    }
}
