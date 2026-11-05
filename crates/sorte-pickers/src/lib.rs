//! The picker contract (design doc, § Modello a plugin dei picker): what every picker
//! implements, so core, store and UI never depend on a particular one, and the suite that
//! checks it.
//!
//! - [`Picker`] and [`Choreography`]: a picker turns an outcome `sorte-core` already decided
//!   into a choreography, a pure function of time that stages it and lands on it. Between
//!   draws the controller keeps the picker's rest state, which gestures ([`Gesture`]) and idle
//!   motion move.
//! - [`Frame`]: the drawing contract with `sorte-stage`, display lists of kurbo and peniko
//!   types (Vello's own), with a motion-blurred moving layer and a sharp top layer, drawn at a
//!   [`Viewport`].
//! - [`TextEngine`]: Parley shaping with pinned font fallbacks, for labels drawn into frames.
//! - [`SpinRequest`], [`TimedEvent`], [`CosmeticRng`], [`ListView`], [`SettingsSchema`],
//!   [`PointerEvent`]: what a picker is given and what it announces.
//! - [`contract`]: the suite every picker crate runs from its own tests.
//!
//! Everything here is backend-free: no wgpu, no Vello, so pickers are tested without a GPU.
#![forbid(unsafe_code)]

pub mod contract;
mod frame;
mod list;
mod localized;
mod picker;
mod pointer;
mod rng;
mod settings;
mod spin;
mod text;

/// The geometry and paint types of [`Frame`], re-exported so pickers draw with the very versions
/// Vello renders.
pub use {kurbo, peniko};

pub use frame::{Cmd, DisplayList, Frame, Glyph, GlyphRun, MotionBlur, Picture, Viewport};
pub use list::{ListView, ViewEntry};
pub use localized::{Language, LocalizedStr};
pub use picker::{Assets, Choreography, OutcomeKind, Picker, PickerError, PickerMeta, Shown};
pub use pointer::{Gesture, PointerEvent, PointerPhase};
pub use rng::CosmeticRng;
pub use settings::{
    BoolField, Choice, ChoiceField, FloatField, IntField, PickerSettings, SettingField, SettingIssue, SettingsSchema,
};
pub use spin::{EventKind, SpinRequest, TimedEvent};
pub use text::{FontError, FontSource, TextEngine, TextLine, TextStyle};
