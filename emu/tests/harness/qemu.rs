//! The QEMU backend, for the i.MX21 machine being added to QEMU: not
//! written yet. Selected with NEOCHUMBY_EMU_BACKEND=qemu, it reports
//! itself unavailable and the scenarios are skipped.
//!
//! What it is to do, against the [`Backend`] and [`Machine`] traits:
//!
//! - **boot:** run `qemu-system-arm` (NEOCHUMBY_EMU_QEMU, else on PATH)
//!   with the Chumby machine, `-kernel` the zImage, the NAND image as the
//!   machine's flash (a copy per boot, as here), `-serial
//!   unix:WORK/serial.sock,server=on,wait=off` and `-qmp
//!   unix:WORK/qmp.sock,server=on,wait=off`, `-display none`. The serial
//!   socket's two halves are [`Booted::console_out`] and `console_in`.
//! - **press / release:** QMP `input-send-event` with `abs` events on the
//!   touchscreen's axes, scaled from screen pixels to QEMU's 0-0x7fff
//!   range, and `btn` `left` down or up.
//! - **tap:** press, wait `hold` of wall time, release. (Unlike
//!   chumby-emu, QEMU's guest time follows the host's, so a wall-clock
//!   hold is a guest hold, unless it runs with -icount.)
//! - **screendump:** QMP `screendump` with `filename`, whose default
//!   format is the PPM the harness reads.
//! - **stop:** QMP `quit`, then wait for the process.

use std::path::Path;

use super::backend::{Backend, Booted, Media, Result};

#[allow(unused_imports)]
use super::backend::Machine;

pub struct Qemu;

impl Backend for Qemu {
    fn name(&self) -> &'static str {
        "qemu"
    }

    fn available(&self) -> Result<()> {
        Err("the QEMU backend is not written yet (tests/harness/qemu.rs)".into())
    }

    fn boot(&self, _media: &Media, _work: &Path) -> Result<Booted> {
        Err("the QEMU backend is not written yet (see tests/harness/qemu.rs)".into())
    }
}
