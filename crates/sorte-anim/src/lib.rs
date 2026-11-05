//! Animation maths shared by the pickers and the stage: easing curves, monotone
//! splines, springs and the vsync-quantized clock. Depends on no other workspace crate.
//!
//! A choreography computes its whole state from the animated time t alone, never from
//! deltas accumulated frame by frame (CLAUDE.md, "Time and choreography"): the same t
//! gives the same frame at any refresh rate. So every curve here is a closed-form
//! function of t, and the clock supplies a t that advances in whole refresh periods.
//!
//! - [`easing`]: the spin's deceleration E(x) = 1 − (1 − x)^n, its slope and its inverse.
//! - [`spline`]: monotone cubic interpolation, so that no horse runs backwards.
//! - [`spring`]: a damped spring in closed form, for the pointer's settle and the race camera.
//! - [`clock`]: the vsync-quantized clock that turns the instants at which frames start into t.
#![forbid(unsafe_code)]

pub mod clock;
pub mod easing;
pub mod spline;
pub mod spring;

/// Time in seconds.
pub type Seconds = f64;
