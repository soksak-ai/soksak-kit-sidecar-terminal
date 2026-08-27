//! The render half of the kit: a sidecar paints its own grid into an IOSurface
//! ring and the application composites the surface — no frame ever crosses a
//! wire. Rust owns the grid snapshot, damage and the ring state machine;
//! `render_darwin.m` owns every Metal, CoreText and IOSurface call. Off macOS
//! every entry refuses by name — there is no software path.

pub mod atlas;
pub mod instances;
pub mod native;
