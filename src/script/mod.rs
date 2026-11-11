//! Scriptable screenshot harness.

pub mod op;
pub mod parser;
pub mod runner;

#[cfg(test)]
mod upload_op_tests;

pub use op::{
    Check, Chord, ConflictChoice, ConflictKindName, DialogExpect, DragMods, EditMode, FeatureKind,
    MouseButton, NativeAppearanceWant, Op, Point2, SelectionKind, Step,
};
pub use parser::{parse, ParseError};
