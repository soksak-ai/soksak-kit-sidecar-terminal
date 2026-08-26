pub mod checkpoint;
pub mod daemon;
pub mod frame;
#[cfg(feature = "integration-tests")]
pub mod integration;
pub mod mirror;
pub mod proto;
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
}

pub type MirrorFactory = fn(cols: u16, rows: u16) -> Box<dyn TerminalStateMirror>;
