pub mod checkpoint;
pub mod daemon;
#[cfg(feature = "integration-tests")]
pub mod integration;
pub mod mirror;
pub mod proto;
pub mod runtime;

pub trait TerminalStateMirror: Send {
    fn feed(&mut self, bytes: &[u8]);
    fn resize(&mut self, cols: u16, rows: u16);
    fn rehydrate(&self) -> Vec<u8>;
    fn cold_paint(&self) -> Vec<u8>;
    fn frame(&self) -> mirror::TerminalFrame;
    fn alt_active(&self) -> bool;
    fn suppressed_replies(&self) -> u64;
}

pub type MirrorFactory = fn(cols: u16, rows: u16) -> Box<dyn TerminalStateMirror>;
