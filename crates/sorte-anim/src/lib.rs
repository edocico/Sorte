//! Animation maths shared by the pickers and the stage: easing curves, monotone
//! splines, springs and the vsync-quantized clock. Depends on no other workspace crate.
#![forbid(unsafe_code)]

/// Time in seconds.
pub type Seconds = f64;
