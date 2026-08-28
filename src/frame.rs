//! The `terminal.frame` wire: rows as runs, per-subscriber baselines, deltas, and the reference
//! `apply` that folds a delta series back into a full picture (SPEC.md §5.1).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::mirror::{
    FrameLine, FrameRun, TerminalCell, TerminalColor, TerminalFrame, TerminalModes, frame_color,
};

/// Baselines kept per pane; beyond this the least recently used is evicted.
pub const MAX_SUBSCRIBERS: usize = 8;

pub const ATTR_BOLD: u16 = 1;
pub const ATTR_DIM: u16 = 2;
pub const ATTR_ITALIC: u16 = 4;
pub const ATTR_UNDERLINE: u16 = 8;
pub const ATTR_INVERSE: u16 = 16;
pub const ATTR_STRIKEOUT: u16 = 32;
pub const ATTR_HIDDEN: u16 = 64;

/// One viewport row as runs. Spacers are dropped; a non-inverse blank shows no fg, bold, dim,
/// italic or hidden and is normalized before runs form; trailing default blanks are trimmed.
pub fn encode_line(y: u16, cells: &[TerminalCell]) -> FrameLine {
    let wrapped = cells.last().is_some_and(|cell| cell.wrapline);
    let mut end = cells.len();
    while end > 0 && is_default_blank(&cells[end - 1]) {
        end -= 1;
    }
    let mut runs: Vec<FrameRun> = Vec::new();
    for cell in cells[..end].iter().filter(|cell| !cell.spacer) {
        let (fg, attrs) = visible_style(cell);
        let bg = frame_color(cell.bg);
        let width = if cell.wide { 2 } else { 1 };
        match runs.last_mut() {
            Some(last)
                if last.fg == fg
                    && last.bg == bg
                    && last.attrs == attrs
                    && last.wide == cell.wide
                    && last.link == cell.link =>
            {
                last.text.push(cell.ch);
                last.text.extend(cell.zerowidth.iter());
                last.n += width;
            }
            _ => {
                let mut text = String::new();
                text.push(cell.ch);
                text.extend(cell.zerowidth.iter());
                runs.push(FrameRun {
                    text,
                    fg,
                    bg,
                    attrs,
                    n: width,
                    wide: cell.wide,
                    link: cell.link.clone(),
                });
            }
        }
    }
    FrameLine { y, wrapped, runs }
}

fn is_blank(cell: &TerminalCell) -> bool {
    cell.ch == ' ' && cell.zerowidth.is_empty() && !cell.wide
}

fn visible_style(cell: &TerminalCell) -> (String, u16) {
    let glyphless = is_blank(cell) && !cell.inverse;
    let fg = if glyphless {
        TerminalColor::Default
    } else {
        cell.fg
    };
    let mut attrs = 0;
    if cell.bold && !glyphless {
        attrs |= ATTR_BOLD;
    }
    if cell.dim && !glyphless {
        attrs |= ATTR_DIM;
    }
    if cell.italic && !glyphless {
        attrs |= ATTR_ITALIC;
    }
    if cell.underline {
        attrs |= ATTR_UNDERLINE;
    }
    if cell.inverse {
        attrs |= ATTR_INVERSE;
    }
    if cell.strikeout {
        attrs |= ATTR_STRIKEOUT;
    }
    if cell.hidden && !glyphless {
        attrs |= ATTR_HIDDEN;
    }
    (frame_color(fg), attrs)
}

fn is_default_blank(cell: &TerminalCell) -> bool {
    is_blank(cell)
        && cell.link.is_none()
        && cell.bg == TerminalColor::Default
        && !cell.underline
        && !cell.strikeout
        && !cell.inverse
}

struct Fnv(u64);

impl Fnv {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Self {
        Self(Self::OFFSET)
    }
    fn byte(&mut self, byte: u8) {
        self.0 ^= u64::from(byte);
        self.0 = self.0.wrapping_mul(Self::PRIME);
    }
    fn bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.byte(*byte);
        }
    }
}

/// FNV-1a 64 over `wrapped` and the run tuples. The cursor is not part of it.
pub fn line_hash(line: &FrameLine) -> u64 {
    let mut hash = Fnv::new();
    hash.byte(u8::from(line.wrapped));
    for run in &line.runs {
        hash.bytes(run.text.as_bytes());
        hash.byte(0xff);
        hash.bytes(run.fg.as_bytes());
        hash.byte(0xff);
        hash.bytes(run.bg.as_bytes());
        hash.byte(0xff);
        hash.bytes(&run.attrs.to_le_bytes());
        hash.bytes(&run.n.to_le_bytes());
        hash.byte(u8::from(run.wide));
        match &run.link {
            Some(link) => {
                hash.byte(1);
                hash.bytes(link.as_bytes());
            }
            None => hash.byte(0),
        }
        hash.byte(0xfe);
    }
    hash.0
}

/// What one subscriber last received: the geometry that forces a full reply and one hash per row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameBaseline {
    pub cols: u16,
    pub rows: u16,
    pub alt_active: bool,
    pub offset: usize,
    pub hashes: Vec<u64>,
    pub last_used: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameReply {
    pub output_sequence: u64,
    pub cols: u16,
    pub rows: u16,
    pub cursor: (usize, usize),
    pub cursor_visible: bool,
    pub cursor_style: crate::mirror::TerminalCursorStyle,
    pub alt_active: bool,
    pub history_size: usize,
    pub offset: usize,
    pub modes: TerminalModes,
    pub full: bool,
    pub lines: Vec<FrameLine>,
}

impl FrameReply {
    pub fn full(frame: &TerminalFrame, output_sequence: u64) -> Self {
        Self {
            output_sequence,
            cols: frame.cols,
            rows: frame.rows,
            cursor: frame.cursor,
            cursor_visible: frame.cursor_visible,
            cursor_style: frame.cursor_style,
            alt_active: frame.alt_active,
            history_size: frame.history_size,
            offset: frame.offset,
            modes: frame.modes,
            full: true,
            lines: frame.lines.clone(),
        }
    }
}

/// The reply for `frame` against `baseline`, and the baseline to keep afterwards. Full when there
/// is no baseline or `(cols, rows, alt_active, offset)` moved; otherwise only rows whose hash
/// changed.
pub fn delta(
    baseline: Option<&FrameBaseline>,
    frame: &TerminalFrame,
    output_sequence: u64,
) -> (FrameReply, FrameBaseline) {
    let hashes: Vec<u64> = frame.lines.iter().map(line_hash).collect();
    let unchanged_geometry = baseline.is_some_and(|previous| {
        previous.cols == frame.cols
            && previous.rows == frame.rows
            && previous.alt_active == frame.alt_active
            && previous.offset == frame.offset
            && previous.hashes.len() == hashes.len()
    });
    let mut reply = FrameReply::full(frame, output_sequence);
    if unchanged_geometry {
        let previous = &baseline.expect("checked above").hashes;
        reply.full = false;
        reply.lines = frame
            .lines
            .iter()
            .zip(&hashes)
            .zip(previous)
            .filter(|((_, now), before)| now != before)
            .map(|((line, _), _)| line.clone())
            .collect();
    }
    let next = FrameBaseline {
        cols: frame.cols,
        rows: frame.rows,
        alt_active: frame.alt_active,
        offset: frame.offset,
        hashes,
        last_used: baseline.map_or(0, |previous| previous.last_used),
    };
    (reply, next)
}

/// The reference fold, shared with the contract and transcribed by viewers: a full reply
/// replaces the picture; a delta replaces every header field and the rows it names.
pub fn apply(previous: &FrameReply, reply: &FrameReply) -> FrameReply {
    let mut applied = reply.clone();
    applied.full = true;
    if reply.full {
        return applied;
    }
    applied.lines = previous.lines.clone();
    for line in &reply.lines {
        match applied.lines.iter_mut().find(|slot| slot.y == line.y) {
            Some(slot) => *slot = line.clone(),
            None => applied.lines.push(line.clone()),
        }
    }
    applied.lines.sort_by_key(|line| line.y);
    applied
}

/// Baselines of one pane keyed by subscriber, capped at `MAX_SUBSCRIBERS` by least recent use.
#[derive(Debug, Default)]
pub struct FrameSubscribers {
    tick: u64,
    baselines: HashMap<String, FrameBaseline>,
}

impl FrameSubscribers {
    pub fn baseline(&self, subscriber: &str) -> Option<&FrameBaseline> {
        self.baselines.get(subscriber)
    }

    pub fn record(&mut self, subscriber: &str, mut baseline: FrameBaseline) {
        self.tick += 1;
        baseline.last_used = self.tick;
        if !self.baselines.contains_key(subscriber) && self.baselines.len() >= MAX_SUBSCRIBERS {
            let oldest = self
                .baselines
                .iter()
                .min_by_key(|(_, kept)| kept.last_used)
                .map(|(name, _)| name.clone());
            if let Some(name) = oldest {
                self.baselines.remove(&name);
            }
        }
        self.baselines.insert(subscriber.to_string(), baseline);
    }

    pub fn len(&self) -> usize {
        self.baselines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.baselines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::{RecoveryMirror, TerminalEngine};
    use crate::mirror::{TerminalCursorShape, TerminalCursorStyle};

    fn cell(ch: char) -> TerminalCell {
        TerminalCell {
            ch,
            fg: TerminalColor::Default,
            bg: TerminalColor::Default,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: false,
            strikeout: false,
            hidden: false,
            wide: false,
            spacer: false,
            wrapline: false,
            zerowidth: Vec::new(),
            link: None,
        }
    }

    fn wide(ch: char) -> [TerminalCell; 2] {
        let mut body = cell(ch);
        body.wide = true;
        let mut spacer = cell(' ');
        spacer.spacer = true;
        [body, spacer]
    }

    fn frame(rows: &[&str]) -> TerminalFrame {
        TerminalFrame {
            cols: 8,
            rows: rows.len() as u16,
            cursor: (0, 0),
            cursor_visible: true,
            cursor_style: TerminalCursorStyle {
                shape: TerminalCursorShape::Block,
                blinking: false,
                blink_interval_ms: 750,
            },
            alt_active: false,
            history_size: 0,
            offset: 0,
            modes: TerminalModes::default(),
            lines: rows
                .iter()
                .enumerate()
                .map(|(y, text)| {
                    let cells: Vec<TerminalCell> = text.chars().map(cell).collect();
                    encode_line(y as u16, &cells)
                })
                .collect(),
        }
    }

    #[test]
    fn encode_line_merges_equal_style_cells_and_counts_wide_as_two() {
        let mut a = cell('a');
        a.bold = true;
        let mut b = cell('b');
        b.bold = true;
        let mut cells = vec![a, b];
        let [mut ga, gs] = wide('가');
        ga.bold = true;
        cells.extend([ga, gs]);
        let [mut na, ns] = wide('나');
        na.bold = true;
        cells.extend([na, ns]);
        cells.push(cell('c'));
        let line = encode_line(3, &cells);
        assert_eq!(line.y, 3);
        assert!(!line.wrapped);
        assert_eq!(line.runs.len(), 3);
        assert_eq!(
            (
                line.runs[0].text.as_str(),
                line.runs[0].n,
                line.runs[0].attrs
            ),
            ("ab", 2, ATTR_BOLD)
        );
        assert_eq!(
            (
                line.runs[1].text.as_str(),
                line.runs[1].n,
                line.runs[1].wide
            ),
            ("가나", 4, true)
        );
        assert_eq!(
            (
                line.runs[2].text.as_str(),
                line.runs[2].n,
                line.runs[2].attrs
            ),
            ("c", 1, 0)
        );
    }

    #[test]
    fn encode_line_drops_spacers_and_trims_trailing_blank_default_cells() {
        let mut cells = vec![cell('x')];
        cells.extend(wide('한'));
        let mut bold_blank = cell(' ');
        bold_blank.bold = true;
        cells.push(bold_blank);
        cells.push(cell(' '));
        let line = encode_line(0, &cells);
        assert_eq!(line.runs.len(), 2);
        assert_eq!(line.runs[0].text, "x");
        assert_eq!((line.runs[1].text.as_str(), line.runs[1].n), ("한", 2));
        assert_eq!(line.runs.iter().map(|run| run.n).sum::<u32>(), 3);

        let mut colored_blank = cell(' ');
        colored_blank.bg = TerminalColor::Named(1);
        let mut wrapped_tail = cell(' ');
        wrapped_tail.wrapline = true;
        let kept = encode_line(
            1,
            &[cell('a'), colored_blank, cell('b'), cell(' '), wrapped_tail],
        );
        assert!(kept.wrapped);
        assert_eq!(kept.runs.len(), 3);
        assert_eq!(
            (kept.runs[1].text.as_str(), kept.runs[1].bg.as_str()),
            (" ", "palette:1")
        );
        assert_eq!(encode_line(2, &[cell(' '); 0]).runs, Vec::<FrameRun>::new());
        assert_eq!(
            encode_line(2, &[cell(' '), cell(' ')]).runs,
            Vec::<FrameRun>::new()
        );
    }

    #[test]
    fn line_hash_distinguishes_link() {
        let plain = encode_line(0, &[cell('a'), cell('b')]);
        let mut linked_a = cell('a');
        linked_a.link = Some("https://example.invalid/".into());
        let mut linked_b = cell('b');
        linked_b.link = linked_a.link.clone();
        let linked = encode_line(0, &[linked_a, linked_b]);
        assert_eq!(plain.runs[0].text, linked.runs[0].text);
        assert_ne!(line_hash(&plain), line_hash(&linked));
        assert_eq!(
            line_hash(&plain),
            line_hash(&encode_line(0, &[cell('a'), cell('b')]))
        );
        let mut wrapped = plain.clone();
        wrapped.wrapped = true;
        assert_ne!(line_hash(&plain), line_hash(&wrapped));
    }

    #[test]
    fn first_request_is_full_then_only_changed_rows() {
        let first = frame(&["one", "two", "three"]);
        let (reply, baseline) = delta(None, &first, 10);
        assert!(reply.full);
        assert_eq!(reply.lines.len(), 3);
        assert_eq!(reply.output_sequence, 10);

        let second = frame(&["one", "TWO", "three"]);
        let (reply, baseline) = delta(Some(&baseline), &second, 11);
        assert!(!reply.full);
        assert_eq!(reply.lines, vec![second.lines[1].clone()]);

        let (reply, _) = delta(Some(&baseline), &second, 12);
        assert!(!reply.full);
        assert!(reply.lines.is_empty());
        assert_eq!(reply.output_sequence, 12);
    }

    #[test]
    fn resize_offset_and_alt_switch_force_full() {
        let base = frame(&["a", "b"]);
        let (_, baseline) = delta(None, &base, 1);

        let mut resized = base.clone();
        resized.cols += 1;
        assert!(delta(Some(&baseline), &resized, 2).0.full);

        let mut scrolled = base.clone();
        scrolled.offset = 1;
        scrolled.history_size = 1;
        assert!(delta(Some(&baseline), &scrolled, 3).0.full);

        let mut alt = base.clone();
        alt.alt_active = true;
        assert!(delta(Some(&baseline), &alt, 4).0.full);

        let mut cursor_only = base.clone();
        cursor_only.cursor = (1, 1);
        let (reply, _) = delta(Some(&baseline), &cursor_only, 5);
        assert!(!reply.full);
        assert!(reply.lines.is_empty());
        assert_eq!(reply.cursor, (1, 1));
    }

    #[test]
    fn baselines_are_capped_by_least_recent_use() {
        let mut table = FrameSubscribers::default();
        let (_, baseline) = delta(None, &frame(&["a"]), 1);
        for index in 0..MAX_SUBSCRIBERS + 1 {
            table.record(&format!("s{index}"), baseline.clone());
        }
        assert_eq!(table.len(), MAX_SUBSCRIBERS);
        assert!(table.baseline("s0").is_none());
        assert!(table.baseline("s1").is_some());
    }

    struct ScriptEngine {
        cols: u16,
        rows: u16,
        lines: Vec<String>,
    }

    impl TerminalEngine for ScriptEngine {
        fn new(cols: u16, rows: u16) -> Self {
            Self {
                cols,
                rows,
                lines: Vec::new(),
            }
        }
        fn feed(&mut self, bytes: &[u8]) {
            for part in String::from_utf8_lossy(bytes).split('\n') {
                self.lines.push(part.to_string());
            }
        }
        fn resize(&mut self, cols: u16, rows: u16) {
            self.cols = cols;
            self.rows = rows;
        }
        fn cols(&self) -> u16 {
            self.cols
        }
        fn rows(&self) -> u16 {
            self.rows
        }
        fn cursor(&self) -> (usize, usize) {
            (
                self.lines.len().min(self.rows as usize).saturating_sub(1),
                0,
            )
        }
        fn cursor_style(&self) -> TerminalCursorStyle {
            TerminalCursorStyle {
                shape: TerminalCursorShape::Block,
                blinking: false,
                blink_interval_ms: 750,
            }
        }
        fn alt_active(&self) -> bool {
            false
        }
        fn history_size(&self) -> usize {
            self.lines.len().saturating_sub(self.rows as usize)
        }
        fn modes(&self) -> TerminalModes {
            TerminalModes {
                show_cursor: true,
                line_wrap: true,
                ..TerminalModes::default()
            }
        }
        fn line_cells(&self, line: i32) -> Vec<TerminalCell> {
            let index = self.history_size() as i32 + line;
            let text = usize::try_from(index)
                .ok()
                .and_then(|index| self.lines.get(index))
                .cloned()
                .unwrap_or_default();
            (0..self.cols as usize)
                .map(|col| cell(text.chars().nth(col).unwrap_or(' ')))
                .collect()
        }
        fn suppressed_replies(&self) -> u64 {
            0
        }
    }

    #[test]
    fn apply_of_deltas_equals_frame_at() {
        let mut mirror = RecoveryMirror::<ScriptEngine>::new(8, 3);
        mirror.feed(b"one\ntwo");
        let (first, baseline) = delta(None, &mirror.frame_at(0), 1);
        mirror.feed(b"three");
        let (second, baseline) = delta(Some(&baseline), &mirror.frame_at(0), 2);
        assert!(!second.full);
        assert_eq!(second.lines.len(), 1, "only the third row changed");
        mirror.feed(b"four\nfive");
        let latest = mirror.frame_at(0);
        let (third, _) = delta(Some(&baseline), &latest, 3);
        assert!(!third.full);
        let applied = apply(&apply(&first, &second), &third);
        assert_eq!(applied, FrameReply::full(&latest, 3));
        assert_eq!(applied.history_size, 2);
        assert_eq!(mirror.frame_at(9).offset, 2, "offset clamps to history");
    }
}
