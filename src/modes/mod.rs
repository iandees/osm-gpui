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

use gpui::{px, CursorStyle, Pixels, Point, Window};
use osm_gpui::coordinates::LatLon;
use osm_gpui::edit_mode::EditMode;

pub(crate) use add::{AddProgress, AddState};
#[cfg(test)]
pub(crate) use arc::ArcPoint;
pub(crate) use arc::{ArcProgress, ArcState};
pub(crate) use building::{BuildingPress, BuildingProgress, BuildingReference, BuildingState};
#[cfg(test)]
pub(crate) use extrude::ExtrudeStyle;
pub(crate) use extrude::{ExtrudeDrag, ExtrudeState};
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

/// `ModeState` accessors for one variant's state, `None` for every other. The
/// other variants are listed so a new mode does not compile until each
/// accessor says what it is for it.
macro_rules! state_accessors {
    ($variant:ident, $ty:ty, $get:ident, $get_mut:ident, [$($other:ident),*]) => {
        impl ModeState {
            fn $get(&self) -> Option<&$ty> {
                match self {
                    ModeState::$variant(s) => Some(s),
                    $(ModeState::$other(_))|* => None,
                }
            }

            fn $get_mut(&mut self) -> Option<&mut $ty> {
                match self {
                    ModeState::$variant(s) => Some(s),
                    $(ModeState::$other(_))|* => None,
                }
            }
        }
    };
}

state_accessors!(
    Add,
    AddState,
    add,
    add_mut,
    [Select, Building, Extrude, Arc, Improve]
);
state_accessors!(
    Building,
    BuildingState,
    building,
    building_mut,
    [Select, Add, Extrude, Arc, Improve]
);
state_accessors!(
    Arc,
    ArcState,
    arc,
    arc_mut,
    [Select, Add, Building, Extrude, Improve]
);
state_accessors!(
    Extrude,
    ExtrudeState,
    extrude,
    extrude_mut,
    [Select, Add, Building, Arc, Improve]
);

/// Accessors for one mode's slot on `MapViewer`: `get` reads it, `take`
/// empties it and `set` fills it. Each is `None` or a no-op outside the
/// slot's mode, where the slot does not exist.
macro_rules! mode_slot {
    ($state:ident, $state_mut:ident, $field:ident, $ty:ty, get $get:ident) => {
        impl crate::MapViewer {
            pub(crate) fn $get(&self) -> Option<&$ty> {
                self.mode_state.$state()?.$field.as_ref()
            }
        }
    };
    ($state:ident, $state_mut:ident, $field:ident, $ty:ty, take $take:ident) => {
        impl crate::MapViewer {
            pub(crate) fn $take(&mut self) -> Option<$ty> {
                self.mode_state.$state_mut()?.$field.take()
            }
        }
    };
    ($state:ident, $state_mut:ident, $field:ident, $ty:ty, set $set:ident) => {
        impl crate::MapViewer {
            pub(crate) fn $set(&mut self, value: Option<$ty>) {
                if let Some(s) = self.mode_state.$state_mut() {
                    s.$field = value;
                }
            }
        }
    };
}

mode_slot!(add, add_mut, progress, AddProgress, get add_progress);
mode_slot!(add, add_mut, progress, AddProgress, take take_add_progress);
mode_slot!(add, add_mut, progress, AddProgress, set set_add_progress);
mode_slot!(building, building_mut, progress, BuildingProgress, get building_progress);
mode_slot!(building, building_mut, progress, BuildingProgress, take take_building_progress);
mode_slot!(building, building_mut, progress, BuildingProgress, set set_building_progress);
mode_slot!(building, building_mut, reference, BuildingReference, get building_reference);
mode_slot!(building, building_mut, reference, BuildingReference, set set_building_reference);
mode_slot!(building, building_mut, press, BuildingPress, take take_building_press);
mode_slot!(building, building_mut, press, BuildingPress, set set_building_press);
mode_slot!(arc, arc_mut, progress, ArcProgress, get arc_progress);
mode_slot!(arc, arc_mut, progress, ArcProgress, take take_arc_progress);
mode_slot!(arc, arc_mut, progress, ArcProgress, set set_arc_progress);
mode_slot!(extrude, extrude_mut, drag, ExtrudeDrag, get extrude_drag);
mode_slot!(extrude, extrude_mut, drag, ExtrudeDrag, take take_extrude_drag);
mode_slot!(extrude, extrude_mut, drag, ExtrudeDrag, set set_extrude_drag);

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

    /// A user's switch to `mode`: a fresh gesture state, plus whatever the
    /// mode carries over from the current state or the selection.
    pub(crate) fn switch_to_mode(&mut self, mode: EditMode) {
        match mode {
            EditMode::Building => building::switch_to(self),
            EditMode::Select
            | EditMode::Add
            | EditMode::Extrude
            | EditMode::Arc
            | EditMode::Improve => self.enter_mode(mode),
        }
    }

    /// Whether a click redraws the map even if the selection did not change.
    /// Add mode re-selects the same way on every extend click, so the
    /// selection diff misses that the way grew a node.
    pub(crate) fn click_always_redraws(&self) -> bool {
        match self.mode_state {
            ModeState::Add(_) => true,
            ModeState::Select(_)
            | ModeState::Building(_)
            | ModeState::Extrude(_)
            | ModeState::Arc(_)
            | ModeState::Improve(_) => false,
        }
    }

    /// Whether the snap highlight follows a pointer that has no button down.
    pub(crate) fn snaps_on_hover(&self) -> bool {
        match self.mode_state {
            ModeState::Add(_) => true,
            ModeState::Select(_)
            | ModeState::Building(_)
            | ModeState::Extrude(_)
            | ModeState::Arc(_)
            | ModeState::Improve(_) => false,
        }
    }

    /// What a pointer hovering at `pos` would snap onto, in the current mode.
    pub(crate) fn hover_snap_target(
        &self,
        pos: Point<Pixels>,
        modifiers: gpui::Modifiers,
    ) -> Option<crate::SnapTarget> {
        match self.mode_state {
            ModeState::Add(_) => add::hover_snap_target(self, pos, modifiers),
            ModeState::Select(_)
            | ModeState::Building(_)
            | ModeState::Extrude(_)
            | ModeState::Arc(_)
            | ModeState::Improve(_) => None,
        }
    }

    /// Whether the mode's preview changes with the held modifier keys.
    pub(crate) fn preview_depends_on_modifiers(&self) -> bool {
        match self.mode_state {
            ModeState::Improve(_) => true,
            ModeState::Select(_)
            | ModeState::Add(_)
            | ModeState::Building(_)
            | ModeState::Extrude(_)
            | ModeState::Arc(_) => false,
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

impl crate::MapViewer {
    /// Compute the far 2 corners of a rectangle whose near edge is
    /// `a`..`b` (geo coordinates), offset by `cursor`'s perpendicular
    /// distance from that edge. The offset math is done in screen-pixel
    /// space rather than raw lat/lon: longitude degrees are foreshortened
    /// relative to latitude degrees (Web Mercator), so a "perpendicular"
    /// vector computed from raw lat/lon deltas is only actually
    /// perpendicular on screen for edges running exactly north-south or
    /// east-west — for any other edge it renders as a trapezoid. Screen
    /// pixels (and Mercator meters) are isotropic here, so doing the offset
    /// there and converting back yields a true rectangle on screen.
    pub(crate) fn rectangle_from_geo_edge(
        &self,
        a: (f64, f64),
        b: (f64, f64),
        cursor: (f64, f64),
    ) -> ((f64, f64), (f64, f64)) {
        let a_screen = self.viewport.geo_to_screen(LatLon::new(a.0, a.1));
        let b_screen = self.viewport.geo_to_screen(LatLon::new(b.0, b.1));
        let cursor_screen = self.viewport.geo_to_screen(LatLon::new(cursor.0, cursor.1));
        let (far_a_screen, far_b_screen) = osm_gpui::selection::rectangle_from_edge(
            (a_screen.x.to_f64(), a_screen.y.to_f64()),
            (b_screen.x.to_f64(), b_screen.y.to_f64()),
            (cursor_screen.x.to_f64(), cursor_screen.y.to_f64()),
        );
        let far_a = self
            .viewport
            .screen_to_geo(gpui::point(
                px(far_a_screen.0 as f32),
                px(far_a_screen.1 as f32),
            ))
            .to_pair();
        let far_b = self
            .viewport
            .screen_to_geo(gpui::point(
                px(far_b_screen.0 as f32),
                px(far_b_screen.1 as f32),
            ))
            .to_pair();
        (far_a, far_b)
    }
}
