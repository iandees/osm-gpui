//! What the map does in each edit mode, one module per mode, plus the key
//! classifier shared by all of them.

#[cfg(test)]
mod escape_tests;
pub(crate) mod keys;
#[cfg(test)]
pub(crate) mod state_tests;

use osm_gpui::edit_mode::EditMode;

use crate::{
    AddProgress, ArcProgress, BuildingPress, BuildingProgress, BuildingReference, ExtrudeDrag,
};

/// The current edit mode together with the state only that mode has. Each
/// variant owns exactly the in-progress gesture of its mode, so switching
/// mode (which builds a fresh variant) cannot leave another mode's gesture
/// behind.
pub(crate) enum ModeState {
    Select(SelectState),
    Add(AddState),
    Building(BuildingState),
    Extrude(ExtrudeState),
    Arc(ArcState),
    Improve(ImproveState),
}

/// Select mode's gestures live in `MapViewer::interaction`, which also serves
/// the other modes' presses, so it holds nothing of its own.
#[derive(Default)]
pub(crate) struct SelectState;

/// `progress` is `None` between continuations: the next click starts a new
/// way.
#[derive(Default)]
pub(crate) struct AddState {
    pub(crate) progress: Option<AddProgress>,
}

#[derive(Default)]
pub(crate) struct BuildingState {
    pub(crate) progress: Option<BuildingProgress>,
    /// The squared way the buildings are aligned to, if any. Not progress: it
    /// outlives each building and goes only with the session.
    pub(crate) reference: Option<BuildingReference>,
    /// The pending left press in oriented Building mode.
    pub(crate) press: Option<BuildingPress>,
}

#[derive(Default)]
pub(crate) struct ExtrudeState {
    pub(crate) drag: Option<ExtrudeDrag>,
}

#[derive(Default)]
pub(crate) struct ArcState {
    pub(crate) progress: Option<ArcProgress>,
}

/// Improve Way mode's target is the selection and its operation the held
/// modifiers, so it has nothing to remember.
#[derive(Default)]
pub(crate) struct ImproveState;

impl ModeState {
    /// The state of `mode` with no gesture started.
    pub(crate) fn fresh(mode: EditMode) -> Self {
        match mode {
            EditMode::Select => ModeState::Select(SelectState),
            EditMode::Add => ModeState::Add(AddState::default()),
            EditMode::Building => ModeState::Building(BuildingState::default()),
            EditMode::Extrude => ModeState::Extrude(ExtrudeState::default()),
            EditMode::Arc => ModeState::Arc(ArcState::default()),
            EditMode::Improve => ModeState::Improve(ImproveState),
        }
    }

    pub(crate) fn mode(&self) -> EditMode {
        match self {
            ModeState::Select(_) => EditMode::Select,
            ModeState::Add(_) => EditMode::Add,
            ModeState::Building(_) => EditMode::Building,
            ModeState::Extrude(_) => EditMode::Extrude,
            ModeState::Arc(_) => EditMode::Arc,
            ModeState::Improve(_) => EditMode::Improve,
        }
    }

    /// Drop the unfinished gesture, staying in the same mode. A Building
    /// session keeps its reference way.
    pub(crate) fn reset_progress(&mut self) {
        match self {
            ModeState::Select(_) | ModeState::Improve(_) => {}
            ModeState::Add(s) => *s = AddState::default(),
            ModeState::Building(s) => {
                s.progress = None;
                s.press = None;
            }
            ModeState::Extrude(s) => *s = ExtrudeState::default(),
            ModeState::Arc(s) => *s = ArcState::default(),
        }
    }

    /// Whether the user is part-way through placing points. An Extrude drag
    /// does not count: it only lasts while the button is held, and an undo
    /// cannot arrive during it.
    pub(crate) fn is_drawing(&self) -> bool {
        match self {
            ModeState::Select(_) | ModeState::Improve(_) | ModeState::Extrude(_) => false,
            ModeState::Add(s) => s.progress.is_some(),
            ModeState::Building(s) => s.progress.is_some(),
            ModeState::Arc(s) => s.progress.is_some(),
        }
    }
}

/// Accessors for one mode's slot on `MapViewer`: `get` reads it, `take`
/// empties it and `set` fills it. Each is `None` or a no-op outside the
/// slot's mode, where the slot does not exist.
macro_rules! mode_slot {
    ($variant:ident, $field:ident, $ty:ty, get $get:ident) => {
        impl crate::MapViewer {
            pub(crate) fn $get(&self) -> Option<&$ty> {
                match &self.mode_state {
                    ModeState::$variant(s) => s.$field.as_ref(),
                    _ => None,
                }
            }
        }
    };
    ($variant:ident, $field:ident, $ty:ty, take $take:ident) => {
        impl crate::MapViewer {
            pub(crate) fn $take(&mut self) -> Option<$ty> {
                match &mut self.mode_state {
                    ModeState::$variant(s) => s.$field.take(),
                    _ => None,
                }
            }
        }
    };
    ($variant:ident, $field:ident, $ty:ty, set $set:ident) => {
        impl crate::MapViewer {
            pub(crate) fn $set(&mut self, value: Option<$ty>) {
                if let ModeState::$variant(s) = &mut self.mode_state {
                    s.$field = value;
                }
            }
        }
    };
}

mode_slot!(Add, progress, AddProgress, get add_progress);
mode_slot!(Add, progress, AddProgress, take take_add_progress);
mode_slot!(Add, progress, AddProgress, set set_add_progress);
mode_slot!(Building, progress, BuildingProgress, get building_progress);
mode_slot!(Building, progress, BuildingProgress, take take_building_progress);
mode_slot!(Building, progress, BuildingProgress, set set_building_progress);
mode_slot!(Building, reference, BuildingReference, get building_reference);
mode_slot!(Building, reference, BuildingReference, set set_building_reference);
mode_slot!(Building, press, BuildingPress, take take_building_press);
mode_slot!(Building, press, BuildingPress, set set_building_press);
mode_slot!(Arc, progress, ArcProgress, get arc_progress);
mode_slot!(Arc, progress, ArcProgress, take take_arc_progress);
mode_slot!(Arc, progress, ArcProgress, set set_arc_progress);
mode_slot!(Extrude, drag, ExtrudeDrag, get extrude_drag);
mode_slot!(Extrude, drag, ExtrudeDrag, take take_extrude_drag);
mode_slot!(Extrude, drag, ExtrudeDrag, set set_extrude_drag);

impl crate::MapViewer {
    pub(crate) fn mode(&self) -> EditMode {
        self.mode_state.mode()
    }

    /// Switch to `mode` with no gesture started, without any of the other
    /// things a user's mode switch does (see `on_set_mode`).
    pub(crate) fn enter_mode(&mut self, mode: EditMode) {
        self.mode_state = ModeState::fresh(mode);
    }
}
