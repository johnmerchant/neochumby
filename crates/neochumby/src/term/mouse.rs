//! The touchscreen as a mouse, for a program on a terminal: what it asked for
//! by xterm's mouse reporting, and what it is sent.

/// What of a touch the program asked to see, by xterm's mouse reporting.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Mouse {
    /// 9 (presses), 1000 (and releases), 1002 (and drags) or 1003 (and any
    /// motion, which a touch only has while pressed); 0, none.
    pub mode: u16,
    /// 1006: SGR's encoding, else X10's.
    pub sgr: bool,
}

impl Mouse {
    /// DECSET (`on`) or DECRST of private mode `mode`, if it is a mouse
    /// mode.
    pub fn set(&mut self, mode: u16, on: bool) {
        match mode {
            9 | 1000 | 1002 | 1003 => self.mode = if on { mode } else { 0 },
            1006 => self.sgr = on,
            _ => {}
        }
    }

    /// What the program is sent for a touch on cell (`col`, `row`), counted
    /// from 0, if it asked for touches like it.
    pub fn report(&self, button: Button, col: usize, row: usize) -> Option<Vec<u8>> {
        let wanted = match button {
            Button::Press => self.mode != 0,
            Button::Release => self.mode >= 1000,
            Button::Drag => self.mode >= 1002,
        };
        if !wanted {
            return None;
        }
        let code = if button == Button::Drag { 32 } else { 0 };
        let (col, row) = (col + 1, row + 1);
        Some(if self.sgr {
            let end = if button == Button::Release { 'm' } else { 'M' };
            format!("\x1b[<{code};{col};{row}{end}").into_bytes()
        } else {
            // X10's bytes stop at 223 columns and rows; the screen has far
            // fewer.
            let code = if button == Button::Release { 3 } else { code };
            let at = |n: usize| (32 + n.min(223)) as u8;
            vec![0x1b, b'[', b'M', 32 + code, at(col), at(row)]
        })
    }
}

/// A touch, as the mouse it is to the program: its left button.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Press,
    Drag,
    Release,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touches_as_the_program_asked() {
        let mut m = Mouse::default();
        assert_eq!(m.report(Button::Press, 3, 4), None);
        m.set(1000, true);
        assert_eq!(m.report(Button::Press, 3, 4).unwrap(), b"\x1b[M $%");
        assert_eq!(m.report(Button::Release, 3, 4).unwrap(), b"\x1b[M#$%");
        assert_eq!(m.report(Button::Drag, 3, 4), None);
        m.set(1002, true);
        m.set(1006, true);
        assert_eq!(m.report(Button::Press, 3, 4).unwrap(), b"\x1b[<0;4;5M");
        assert_eq!(m.report(Button::Drag, 63, 29).unwrap(), b"\x1b[<32;64;30M");
        assert_eq!(m.report(Button::Release, 3, 4).unwrap(), b"\x1b[<0;4;5m");
        m.set(1002, false);
        assert_eq!(m.mode, 0);
    }
}
