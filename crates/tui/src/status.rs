//! Reports the agent's state to the terminal host with OSC 7501 records.
//!
//! herdr-core reads these to tell when a turn is working, done, or failed. The
//! record is an escape sequence with no visible effect, so it can be written
//! between frames without disturbing the screen.

use std::io::{self, Write};

/// The state of one agent turn, as herdr-core understands it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    /// A message was submitted and the agent is running.
    Working,
    /// The turn finished and the agent is ready for more input.
    Done,
    /// The turn was cancelled by the user.
    Idle,
    /// The turn failed.
    Error,
}

impl TurnState {
    fn word(self) -> &'static str {
        match self {
            TurnState::Working => "working",
            TurnState::Done => "done",
            TurnState::Idle => "idle",
            TurnState::Error => "error",
        }
    }
}

/// The name herdr-core shows for this program in its `agent` field.
const APP: &str = "cherrypi";

/// The OSC 7501 record for `state`: `ESC ] 7501 ; state=<word>:app=cherrypi BEL`.
/// The `app` key is what sets the agent label in herdr-core's listings.
pub fn sequence(state: TurnState) -> String {
    format!("\x1b]7501;state={}:app={}\x07", state.word(), APP)
}

/// Write the record for `state` to `out`.
pub fn write_state<W: Write>(out: &mut W, state: TurnState) -> io::Result<()> {
    out.write_all(sequence(state).as_bytes())?;
    out.flush()
}

/// Report `state` on this process's standard output. A failed write is ignored,
/// since status is informational and must never break the UI.
pub fn report(state: TurnState) {
    let _ = write_state(&mut io::stdout(), state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_each_state_as_an_osc_7501_record() {
        assert_eq!(sequence(TurnState::Working), "\x1b]7501;state=working:app=cherrypi\x07");
        assert_eq!(sequence(TurnState::Done), "\x1b]7501;state=done:app=cherrypi\x07");
        assert_eq!(sequence(TurnState::Idle), "\x1b]7501;state=idle:app=cherrypi\x07");
        assert_eq!(sequence(TurnState::Error), "\x1b]7501;state=error:app=cherrypi\x07");
    }

    #[test]
    fn writes_the_record_to_the_output() {
        let mut out = Vec::new();
        write_state(&mut out, TurnState::Working).unwrap();
        assert_eq!(out, b"\x1b]7501;state=working:app=cherrypi\x07");
    }
}
