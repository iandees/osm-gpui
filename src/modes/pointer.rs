//! Who handles a pointer event on the map. Several things can claim a left
//! press before any edit mode sees it; this is the one place that orders
//! them:
//!
//! 1. a press that only closed an open in-window menu is swallowed (and so is
//!    its release),
//! 2. a Rotate in progress is applied by the release, not by a press,
//! 3. a held Space turns the drag into a pan,
//! 4. the imagery offset adjustment takes the press to move the imagery,
//! 5. otherwise the current mode (`MapViewer::mode_press`).

use gpui::{CursorStyle, Pixels, Point};
use osm_gpui::edit_mode::EditMode;
use osm_gpui::interaction::Interaction;

use crate::MapViewer;

/// Who owns a left press, by the first of the cross-mode claims that applies.
/// The imagery offset claim (4) depends on where the press lands, so it is
/// decided by `MapViewer::handle_map_mouse_down` once the press is the map's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PressOwner {
    /// It only closed a menu: nothing else sees it.
    Swallowed,
    /// A Rotate is in progress: the release applies it.
    Rotate,
    /// Space is held: pan the map.
    Pan,
    /// The map's own handling: the offset adjustment, else the current mode.
    Map,
}

pub(crate) fn press_owner(menu_swallow: bool, rotating: bool, space_held: bool) -> PressOwner {
    if menu_swallow {
        PressOwner::Swallowed
    } else if rotating {
        PressOwner::Rotate
    } else if space_held {
        PressOwner::Pan
    } else {
        PressOwner::Map
    }
}

/// The mouse cursor to show over the map: a closed hand while pan-dragging,
/// an open hand while Space arms a pan, else the current mode's cursor.
pub(crate) fn map_cursor(
    mode: EditMode,
    panning: bool,
    space_held: bool,
    hovering: bool,
) -> CursorStyle {
    if panning {
        CursorStyle::ClosedHand
    } else if space_held {
        CursorStyle::OpenHand
    } else {
        super::cursor(mode, hovering)
    }
}

impl MapViewer {
    /// The left button went down on the map.
    pub(crate) fn on_left_press(
        &mut self,
        ev: &gpui::MouseDownEvent,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let owner = press_owner(
            self.pointer.menu_swallow_press,
            self.pointer.rotate_session.is_some(),
            self.pointer.space_held,
        );
        if owner == PressOwner::Swallowed {
            return;
        }
        window.focus(&self.focus_handle, cx);
        let position = self.window_to_map(ev.position);
        match owner {
            PressOwner::Swallowed | PressOwner::Rotate => {}
            PressOwner::Pan => self.start_pan_drag(position, cx),
            PressOwner::Map => self.handle_map_mouse_down(position, ev.modifiers),
        }
    }

    /// The left button came up over the map.
    pub(crate) fn on_left_release(
        &mut self,
        ev: &gpui::MouseUpEvent,
        cx: &mut gpui::Context<Self>,
    ) {
        if std::mem::take(&mut self.pointer.menu_swallow_press) {
            return;
        }
        self.handle_mouse_up(ev, cx);
    }

    /// Left-button mouse-down on the map that no cross-mode owner took.
    /// `position` is in map-local coordinates (see `window_to_map`).
    pub(crate) fn handle_map_mouse_down(
        &mut self,
        position: Point<Pixels>,
        modifiers: gpui::Modifiers,
    ) {
        // Adjusting the imagery offset: the press starts moving the imagery.
        if self.offset_adjust_mouse_down(position) {
            self.pointer.interaction = Interaction::Idle;
            return;
        }
        self.mode_press(position, modifiers);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_menu_closing_press_beats_everything() {
        for rotating in [false, true] {
            for space in [false, true] {
                assert_eq!(
                    press_owner(true, rotating, space),
                    PressOwner::Swallowed,
                    "rotating {rotating}, space {space}"
                );
            }
        }
    }

    #[test]
    fn a_rotate_beats_a_space_pan() {
        assert_eq!(press_owner(false, true, true), PressOwner::Rotate);
        assert_eq!(press_owner(false, true, false), PressOwner::Rotate);
    }

    #[test]
    fn space_pans_when_nothing_older_claims_the_press() {
        assert_eq!(press_owner(false, false, true), PressOwner::Pan);
    }

    #[test]
    fn otherwise_the_map_handles_it() {
        assert_eq!(press_owner(false, false, false), PressOwner::Map);
    }

    #[test]
    fn panning_and_space_override_the_modes_cursor() {
        for mode in EditMode::ALL {
            assert_eq!(map_cursor(mode, true, true, true), CursorStyle::ClosedHand);
            assert_eq!(map_cursor(mode, false, true, true), CursorStyle::OpenHand);
        }
    }

    #[test]
    fn each_mode_has_its_cursor() {
        let at_rest = |mode| map_cursor(mode, false, false, false);
        assert_eq!(at_rest(EditMode::Select), CursorStyle::Arrow);
        assert_eq!(at_rest(EditMode::Extrude), CursorStyle::Arrow);
        for mode in [
            EditMode::Add,
            EditMode::Building,
            EditMode::Arc,
            EditMode::Improve,
        ] {
            assert_eq!(at_rest(mode), CursorStyle::Crosshair, "{mode:?}");
        }
        assert_eq!(
            map_cursor(EditMode::Select, false, false, true),
            CursorStyle::PointingHand
        );
        assert_eq!(
            map_cursor(EditMode::Add, false, false, true),
            CursorStyle::Crosshair
        );
    }
}
