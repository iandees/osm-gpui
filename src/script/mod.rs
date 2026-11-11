//! Scriptable screenshot harness.

pub mod op;
pub mod parser;
pub mod runner;

#[cfg(test)]
mod autosave_op_tests;
#[cfg(test)]
mod changeset_op_tests;
#[cfg(test)]
mod upload_op_tests;

pub use op::{
    AutosaveCmd, Check, Chord, ConflictChoice, ConflictKindName, DialogExpect, DragMods, EditMode,
    FeatureKind, FindingTarget, LayerKind, MouseButton, NativeAppearanceWant, Op, Point2,
    SelectionKind, Step, UploadDialogCmd,
};
pub use parser::{parse, ParseError};
