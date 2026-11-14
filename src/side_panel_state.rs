//! Side-panel UI state that outlives a single render: the Fields section's
//! widgets and the in-place editors of the Tags table and Members list.

use crate::{tag_suggest_ui, InlineTagEdit, MemberRef, PendingTagEditOpen, RoleEdit};

/// The Fields section.
#[derive(Default)]
pub(crate) struct FieldsState {
    /// The feature whose Fields section has its "Add field" list expanded.
    /// Keyed by feature so the list is closed again for any other selection
    /// without every selection-changing path having to reset it.
    pub(crate) fields_more_open_for: Option<osm_gpui::selection::FeatureRef>,
    /// Live `InputState` entities for the Fields section's text widgets,
    /// keyed by field id. Rebuilt whenever the selected feature changes so
    /// stale entities from a previous feature never leak into a new one.
    pub(crate) fields_text_inputs:
        std::collections::HashMap<String, gpui::Entity<gpui_component::input::InputState>>,
    /// Field ids that already have a `cx.subscribe` registered on their
    /// `fields_text_inputs` entity, so `text_field_input` never subscribes
    /// twice for the same field across re-renders. Cleared alongside
    /// `fields_text_inputs`.
    pub(crate) fields_text_subscribed: std::collections::HashSet<String>,
    /// Which field's combo/multiCombo option list is currently expanded,
    /// if any (`None` = all collapsed). Only one at a time.
    pub(crate) fields_open_combo: Option<String>,
    /// Field ids promoted from a preset's `more_fields` into the visible
    /// list for the current editing session. Cleared on selection change.
    pub(crate) fields_promoted_more_fields: std::collections::HashSet<String>,
}

/// In-place editing in the Tags table and the Members list.
pub(crate) struct TagEditState {
    /// The tag currently being edited in place in the Tags table, if any.
    pub(crate) tag_inline_edit: Option<InlineTagEdit>,
    /// A request to start an inline tag edit, recorded by a cell/button
    /// click and acted on during the next `render()` — see
    /// `PendingTagEditOpen`'s doc comment.
    pub(crate) pending_tag_edit_open: Option<PendingTagEditOpen>,
    /// Autocomplete for the tag inputs: layer indexes, taginfo bookkeeping.
    pub(crate) tag_suggest: tag_suggest_ui::TagSuggestState,
    /// The role of a relation member being edited in place in the Members
    /// list, if any.
    pub(crate) role_edit: Option<RoleEdit>,
    /// A request to start a role edit, acted on during the next `render()`
    /// for the same reason as `pending_tag_edit_open`.
    pub(crate) pending_role_edit_open: Option<(MemberRef, String)>,
    /// Whether the Members list shows every member of a long relation
    /// instead of the first few.
    pub(crate) members_show_all: bool,
    /// One focus handle per Members row, grouping that row's buttons so the
    /// row can tell whether focus is inside it (grown lazily while rendering).
    pub(crate) member_controls_focus: std::cell::RefCell<Vec<gpui::FocusHandle>>,
}

impl Default for TagEditState {
    fn default() -> Self {
        Self {
            tag_inline_edit: None,
            pending_tag_edit_open: None,
            tag_suggest: tag_suggest_ui::TagSuggestState::new(),
            role_edit: None,
            pending_role_edit_open: None,
            members_show_all: false,
            member_controls_focus: std::cell::RefCell::new(Vec::new()),
        }
    }
}
