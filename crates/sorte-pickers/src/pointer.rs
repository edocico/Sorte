//! Pointer input over the scene, for pickers that react to gestures (dragging the wheel), and
//! what a gesture does to the picker's rest state.

use kurbo::Point;
use sorte_anim::Seconds;

use crate::frame::Viewport;

/// A pointer event over the scene.
///
/// It carries the scene's viewport, so a picker locates its own geometry (the wheel's centre)
/// the same way it draws it, from the size alone, even before a resize reaches `prepare`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerEvent {
    pub phase: PointerPhase,
    /// In the scene's physical pixels: the coordinates of [`Frame`](crate::Frame).
    pub position: Point,
    /// The scene's size, as the next frame will have it.
    pub viewport: Viewport,
    /// A monotonic timestamp: only differences between events matter.
    pub t: Seconds,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerPhase {
    /// A button went down.
    Down,
    /// The pointer moved, with or without a button down.
    Move,
    /// The button went up: the moment a drag becomes a fling.
    Up,
    /// The gesture was taken away (the window lost the pointer): no fling follows.
    Cancel,
}

/// What a pointer event does to the picker at rest, answered by
/// [`Picker::on_pointer`](crate::Picker::on_pointer).
///
/// The controller keeps the rest state, as it does after a draw: the picker draws whatever
/// rest it is given and never keeps a dragged one of its own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gesture {
    /// Nothing changes.
    None,
    /// The pointer moved the picker (the wheel follows the drag): `rest` is the new state on
    /// screen, finite.
    Drag { rest: f64 },
    /// The release flung the picker: the controller draws an outcome and stages it from `rest`
    /// with a release velocity of `omega` per second (for the wheel, rad/s), both finite. Only
    /// a release ([`PointerPhase::Up`]) may fling.
    Fling { rest: f64, omega: f64 },
}
