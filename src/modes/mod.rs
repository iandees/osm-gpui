//! What the map does in each edit mode, one module per mode, plus the key
//! classifier and the pointer arbitration shared by all of them.
//!
//! Every mode module provides the hooks that apply to it, with the same names:
//! `press`, `release`, `click`, `escape`, `cursor` and `paint_preview`, and
//! `is_drawing` on its state. The `impl MapViewer` block below dispatches each
//! hook with a `match` on `ModeState` that names every variant, so a new mode
//! does not compile until every dispatch has an arm for it.

pub(crate) mod add;
pub(crate) mod arc;
pub(crate) mod building;
#[cfg(test)]
mod escape_tests;
pub(crate) mod extrude;
pub(crate) mod improve;
pub(crate) mod keys;
pub(crate) mod pointer;
mod preview;
pub(crate) mod select;
#[cfg(test)]
pub(crate) mod state_tests;

use gpui::{CursorStyle, Pixels, Point, Window};
use osm_gpui::edit_mode::EditMode;

pub(crate) use add::{AddProgress, AddState};
pub(crate) use arc::{ArcPoint, ArcProgress, ArcSnap, ArcState};
pub(crate) use building::{BuildingPress, BuildingProgress, BuildingReference, BuildingState};
pub(crate) use extrude::{ExtrudeDrag, ExtrudeResult, ExtrudeState, ExtrudeStyle};
pub(crate) use improve::ImproveState;
pub(crate) use preview::PreviewFrame;
pub(crate) use select::SelectState;

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
            ModeState::Building(s) => s.reset_progress(),
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
            ModeState::Add(s) => s.is_drawing(),
            ModeState::Building(s) => s.is_drawing(),
            ModeState::Arc(s) => s.is_drawing(),
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

/// The mouse cursor over the map in `mode`.
pub(crate) fn cursor(mode: EditMode, hovering: bool) -> CursorStyle {
    match mode {
        EditMode::Select => select::cursor(hovering),
        EditMode::Add => add::cursor(),
        EditMode::Building => building::cursor(),
        EditMode::Extrude => extrude::cursor(),
        EditMode::Arc => arc::cursor(),
        EditMode::Improve => improve::cursor(),
    }
}

impl crate::MapViewer {
    pub(crate) fn mode(&self) -> EditMode {
        self.mode_state.mode()
    }

    /// Switch to `mode` with no gesture started, without any of the other
    /// things a user's mode switch does (see `on_set_mode`).
    pub(crate) fn enter_mode(&mut self, mode: EditMode) {
        self.mode_state = ModeState::fresh(mode);
    }

    /// A left press on the map that no cross-mode owner took (see
    /// `pointer::press_owner`). A mode that owns the left button takes the
    /// press; for any other the shared select/box-select/move gesture starts.
    pub(crate) fn mode_press(&mut self, position: Point<Pixels>, modifiers: gpui::Modifiers) {
        let took_press = match self.mode_state {
            ModeState::Building(_) => building::press(self, position),
            ModeState::Extrude(_) => {
                extrude::press(self, position, modifiers);
                true
            }
            ModeState::Select(_)
            | ModeState::Add(_)
            | ModeState::Arc(_)
            | ModeState::Improve(_) => false,
        };
        if !took_press {
            let grab_vertices = matches!(self.mode_state, ModeState::Select(_));
            select::press_gesture(self, position, modifiers, grab_vertices);
        }
    }

    /// A left release whose press a mode owned. Returns whether it finished
    /// such a press (the shared gesture then has nothing to do).
    pub(crate) fn mode_release(&mut self, click_count: usize, up_pos: Point<Pixels>) -> bool {
        match self.mode_state {
            ModeState::Building(_) => building::release(self, up_pos),
            ModeState::Extrude(_) => extrude::release(self, click_count, up_pos),
            ModeState::Select(_)
            | ModeState::Add(_)
            | ModeState::Arc(_)
            | ModeState::Improve(_) => false,
        }
    }

    /// Whether the pointer's hover highlight and vertex preview are live.
    pub(crate) fn hover_tracking(&self, button_down: bool) -> bool {
        match self.mode_state {
            ModeState::Select(_) => select::hover_enabled(self, button_down),
            ModeState::Add(_)
            | ModeState::Building(_)
            | ModeState::Extrude(_)
            | ModeState::Arc(_)
            | ModeState::Improve(_) => false,
        }
    }

    /// A plain click on the map (after notes and street imagery had their
    /// say), by the current mode.
    pub(crate) fn mode_click(
        &mut self,
        at: Point<Pixels>,
        shift_held: bool,
        click_count: usize,
        snapping_off: bool,
        cx: &mut gpui::Context<Self>,
    ) {
        match self.mode_state {
            ModeState::Select(_) => select::click(self, at, shift_held, click_count),
            ModeState::Add(_) => add::click(self, at, snapping_off),
            ModeState::Building(_) => building::click(self, at),
            ModeState::Arc(_) => arc::click(self, at, snapping_off),
            ModeState::Improve(_) => improve::click(self, at, cx),
            ModeState::Extrude(_) => extrude::click(),
        }
    }

    /// Escape's step in the current mode: the unfinished gesture first, then
    /// the mode itself. `cancelled_drag` says Escape already cancelled a
    /// move-drag.
    pub(crate) fn mode_escape(&mut self, cancelled_drag: bool) {
        match self.mode_state {
            ModeState::Select(_) => select::escape(self, cancelled_drag),
            ModeState::Add(_) => add::escape(self),
            ModeState::Building(_) => building::escape(self),
            ModeState::Extrude(_) => extrude::escape(self),
            ModeState::Arc(_) => arc::escape(self),
            ModeState::Improve(_) => improve::escape(self),
        }
    }

    /// Paint the current mode's live preview, and the snap highlight. The Add
    /// rubber band goes over the highlight; every other preview goes under
    /// it.
    pub(crate) fn paint_mode_preview(&self, frame: &PreviewFrame<'_>, window: &mut Window) {
        let paint_snap = |window: &mut Window| {
            if let Some(target) = &self.snap_preview {
                preview::paint_snap_target(target, frame, window);
            }
        };
        match self.mode_state {
            ModeState::Add(_) => {
                paint_snap(window);
                add::paint_preview(self, frame, window);
            }
            ModeState::Select(_) => {
                select::paint_preview(self, frame, window);
                paint_snap(window);
            }
            ModeState::Building(_) => {
                building::paint_preview(self, frame, window);
                paint_snap(window);
            }
            ModeState::Extrude(_) => {
                extrude::paint_preview(self, frame, window);
                paint_snap(window);
            }
            ModeState::Arc(_) => {
                arc::paint_preview(self, frame, window);
                paint_snap(window);
            }
            ModeState::Improve(_) => {
                improve::paint_preview(self, frame, window);
                paint_snap(window);
            }
        }
    }
}
