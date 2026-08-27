//! Three surfaces rotate under the contract's ring rule: never paint what was
//! not released, never release what is displayed. The state machine itself
//! lives in the contract crate — this module only binds it to real surfaces.

use soksak_contract_surface::{RingState, RING_SIZE};

use super::native::{Canvas, Surface};
use super::painter::TargetState;

pub struct SurfaceRing {
    slots: Vec<Surface>,
    targets: Vec<TargetState>,
    state: RingState,
    seq: u64,
}

impl SurfaceRing {
    pub fn new(canvas: &Canvas, width: u32, height: u32, rows: u16) -> Result<Self, String> {
        let mut slots = Vec::with_capacity(RING_SIZE);
        let mut targets = Vec::with_capacity(RING_SIZE);
        for _ in 0..RING_SIZE {
            slots.push(canvas.surface(width, height)?);
            targets.push(TargetState::new(rows));
        }
        Ok(Self { slots, targets, state: RingState::new(), seq: 0 })
    }

    /// One fresh send right per slot, in ring order, for the hello's ring
    /// message. Minted once; the application holds the lookup.
    pub fn mach_ports(&self) -> Result<Vec<u32>, String> {
        self.slots.iter().map(|slot| slot.mach_port()).collect()
    }

    pub fn acquire(&mut self) -> Result<usize, String> {
        self.state.acquire_for_render()
    }

    pub fn slot(&self, index: usize) -> &Surface {
        &self.slots[index]
    }

    pub fn target(&mut self, index: usize) -> (&Surface, &mut TargetState) {
        (&self.slots[index], &mut self.targets[index])
    }

    /// The painted slot becomes ready and takes the next frame sequence.
    pub fn signal(&mut self, index: usize) -> u64 {
        self.state.signal(index);
        self.seq += 1;
        self.seq
    }

    /// The application moved onto this slot (implied by releasing another).
    pub fn shown(&mut self, index: usize) -> Result<(), String> {
        self.state.display(index)
    }

    /// The application returned this slot; it is paintable again.
    pub fn released(&mut self, index: usize) -> Result<(), String> {
        self.state.release(index)
    }
}
