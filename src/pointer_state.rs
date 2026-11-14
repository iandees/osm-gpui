//! What the mouse and keyboard modifiers are doing on the map right now.

use crate::{ContextMenuState, RotateSession, SnapTarget};
use osm_gpui::interaction::Interaction;

pub(crate) struct PointerState {
    /// Whether Space is currently held with the map focused; a left-drag
    /// then pans instead of selecting.
    pub(crate) space_held: bool,
    /// Current mouse interaction: idle, a pending press, an in-progress
    /// box-select, or an in-progress move-drag of the selection. See
    /// `osm_gpui::interaction` for the pure click/drag/box-select decision
    /// tree this drives.
    pub(crate) interaction: Interaction,
    /// What the pointer would snap onto right now (Add mode hover, or a
    /// single node being dragged), drawn as a highlight. See `SnapTarget`.
    pub(crate) snap_preview: Option<SnapTarget>,
    /// The modifier keys as of the last mouse move, click or modifier change;
    /// Improve Way mode reads it to preview and pick its operation.
    pub(crate) last_modifiers: gpui::Modifiers,
    /// Last known mouse position within the map area, updated on every
    /// mouse-move; used to drive the live Building-mode preview (which
    /// needs a cursor position outside of a drag) during `render()`.
    pub(crate) last_mouse_pos: Option<gpui::Point<gpui::Pixels>>,
    /// The feature under the cursor that a click would select, if any
    /// (Select mode only). Drawn with the hover highlight.
    pub(crate) hovered: Option<osm_gpui::selection::FeatureRef>,
    /// A visible vertex (previewed, or on a selected way) that was pressed
    /// without being selected: the move-drag it started moves just this node,
    /// and the node becomes the selection once the drag passes the threshold
    /// (a release without moving is a plain click and selects it that way).
    pub(crate) vertex_grab: Option<osm_gpui::selection::FeatureRef>,
    /// The interactive Rotate in progress, if any (see `RotateSession`).
    pub(crate) rotate_session: Option<RotateSession>,
    /// Window position of the last right-button press on the map, kept until
    /// the button is released: a release within `CLICK_SLOP` of it is a click
    /// (opens the context menu), anything further was a pan.
    pub(crate) right_press: Option<gpui::Point<gpui::Pixels>>,
    /// The left press now in progress began while an in-window menu was open
    /// (so it only closes the menu): the map ignores it and its release.
    pub(crate) menu_swallow_press: bool,
    /// The open right-click context menu, if any.
    pub(crate) context_menu: Option<ContextMenuState>,
}

impl Default for PointerState {
    fn default() -> Self {
        Self {
            space_held: false,
            interaction: Interaction::Idle,
            snap_preview: None,
            last_modifiers: gpui::Modifiers::none(),
            last_mouse_pos: None,
            hovered: None,
            vertex_grab: None,
            rotate_session: None,
            right_press: None,
            menu_swallow_press: false,
            context_menu: None,
        }
    }
}
