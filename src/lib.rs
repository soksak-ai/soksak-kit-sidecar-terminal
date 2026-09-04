pub mod checkpoint;
pub mod daemon;
pub mod frame;
#[cfg(feature = "integration-tests")]
pub mod integration;
pub mod mirror;
pub mod modes;
pub mod proto;
pub mod render;
pub mod runtime;
mod transport_name;

pub trait TerminalStateMirror: Send {
    fn feed(&mut self, bytes: &[u8]);
    fn resize(&mut self, cols: u16, rows: u16);
    fn rehydrate(&self) -> Vec<u8>;
    fn cold_paint(&self) -> Vec<u8>;
    /// The viewport scrolled `offset` rows into history; 0 is the bottom.
    fn frame_at(&self, offset: usize) -> mirror::TerminalFrame;
    fn frame(&self) -> mirror::TerminalFrame {
        self.frame_at(0)
    }
    fn history_size(&self) -> usize;
    fn modes(&self) -> mirror::TerminalModes;
    fn capabilities(&self) -> mirror::MirrorCapabilities;
    fn alt_active(&self) -> bool;
    fn suppressed_replies(&self) -> u64;
    /// Viewport geometry and cells, for the painter that renders this mirror.
    fn cols(&self) -> u16;
    fn rows(&self) -> u16;
    /// `(row, col)`, 0-based, in the viewport.
    fn cursor(&self) -> (usize, usize);
    /// Cursor shape/blink as interpreted by the engine.
    fn cursor_style(&self) -> mirror::TerminalCursorStyle;
    /// Provider/user animation policy, separate from terminal cursor state.
    fn cursor_animation(&self) -> mirror::TerminalCursorAnimation;
    /// OSC 4/10/11/12 state interpreted by the engine. Null entries mean the current host base.
    fn theme_overrides(&self) -> mirror::TerminalThemeOverrides {
        mirror::TerminalThemeOverrides::default()
    }
    /// The cells of one viewport row; negative rows reach into history.
    fn line_cells(&self, line: i32) -> Vec<crate::mirror::TerminalCell>;
    fn selection_command(
        &mut self,
        request: &crate::mirror::SelectionRequest,
        offset: usize,
    ) -> Result<crate::mirror::SelectionSnapshot, String>;
    fn selection_range(&self, line: i32) -> Option<(u16, u16)>;
    fn wheel_input(
        &mut self,
        input: crate::mirror::EngineWheelInput,
    ) -> Result<Vec<u8>, String>;
    fn pointer_input(
        &mut self,
        input: crate::mirror::EnginePointerInput,
    ) -> Result<Vec<u8>, String>;
}

pub type MirrorFactory = fn(cols: u16, rows: u16) -> Box<dyn TerminalStateMirror>;
