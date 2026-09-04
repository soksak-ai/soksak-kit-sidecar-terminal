//! The mode state a replay cannot rebuild, between the mirror and the session owner.
//!
//! A mode set before the retained output begins is in no byte the store holds. A rotation drops the
//! half that set it, and a replay then draws into a mirror in the mode it started in rather than the
//! one the session was left in — the alternate screen being the visible one (SESSION.md S4-5).
//!
//! The owner stores the report and reads none of it: what a mode is belongs to whatever parses the
//! output, and the owner parses none.
//!
//! The report carries the thirteen modes `soksak-contract-terminal` declares. A mirror that tracks
//! more than those — DEC private 9 and 1001 are tracked here and are not in the contract's set —
//! restores without them. Extending the set is a change to the contract's reference states, not to
//! this file.

use soksak_contract_terminal::{ModeReport, Modes};

use crate::mirror::TerminalModes;

/// The report for one mirror, in the form the owner stores.
pub fn report_of(modes: TerminalModes, alt: bool) -> ModeReport {
    ModeReport::of(
        Modes {
            bracketed_paste: modes.bracketed_paste,
            app_cursor: modes.app_cursor,
            app_keypad: modes.app_keypad,
            mouse_click: modes.mouse_click,
            mouse_drag: modes.mouse_drag,
            mouse_motion: modes.mouse_motion,
            sgr_mouse: modes.sgr_mouse,
            utf8_mouse: modes.utf8_mouse,
            focus_in_out: modes.focus_in_out,
            alternate_scroll: modes.alternate_scroll,
            show_cursor: modes.show_cursor,
            line_wrap: modes.line_wrap,
            insert: modes.insert,
        },
        alt,
    )
}

/// The bytes that put a fresh mirror into a stored report's modes, or none when the report is empty
/// or was written in a form this build does not read.
///
/// A report this build cannot read is refused rather than read as defaults: defaults restore the
/// modes of a session that never ran and say nothing about it.
pub fn apply_bytes_of(stored: &[u8]) -> Option<Vec<u8>> {
    if stored.is_empty() {
        return None;
    }
    ModeReport::decode(stored).map(|report| report.apply_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes_with_paste() -> TerminalModes {
        TerminalModes { bracketed_paste: true, ..TerminalModes::default() }
    }

    #[test]
    fn a_report_round_trips_through_the_stored_form() {
        let report = report_of(modes_with_paste(), true);
        let decoded = ModeReport::decode(&report.encode()).expect("the stored form reads back");
        assert_eq!(decoded, report);
    }

    #[test]
    fn an_empty_store_applies_nothing() {
        assert_eq!(apply_bytes_of(&[]), None);
    }

    #[test]
    fn a_report_this_build_cannot_read_applies_nothing() {
        assert_eq!(apply_bytes_of(b"v2 1 0"), None);
    }

    #[test]
    fn a_stored_report_applies_the_modes_it_names() {
        let stored = report_of(modes_with_paste(), false).encode();
        let bytes = apply_bytes_of(&stored).expect("a readable report applies");
        assert!(bytes.windows(8).any(|w| w == b"\x1b[?2004h"), "bracketed paste is set");
        assert!(!bytes.windows(9).any(|w| w == b"\x1b[?1049h"), "the primary screen stays");
    }
}
