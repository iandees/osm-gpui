//! The app side of Copy, Paste, Paste at Source Position and Duplicate: the
//! feature clipboard, whether each action applies, and running them on the
//! map view. The copying and pasting itself is `osm_gpui::feature_clipboard`.
//!
//! They are entries of `selection_actions::ACTIONS` (so the Edit menu, the
//! context menu and the keyboard shortcuts treat them like any other action),
//! but `MapViewer::run_selection_action` calls [`MapViewer::run_clipboard_op`]
//! for them instead of `apply`, because they need the pointer position and
//! the system clipboard, which an `ApplyFn` does not get.

use std::cell::RefCell;

use gpui::Context;
use osm_gpui::feature_clipboard::{FeatureClipboard, PastePosition};
use osm_gpui::layers::{LayerId, LayerManager};
use osm_gpui::limits::Limits;
use osm_gpui::selection::FeatureRef;
use osm_gpui::{settings_store, tr, tr_count};

use crate::selection_actions::{ActionEnv, ActionOutcome};
use crate::undo::UndoableAction;
use crate::MapViewer;

/// The most objects whose OSM XML is also put on the system clipboard; a
/// bigger copy stays app-internal, as building the text would stall the UI.
const MAX_OBJECTS_AS_TEXT: usize = 20_000;

thread_local! {
    /// What Copy set aside, for Paste: the app's own feature clipboard. Like
    /// the tag clipboard it is only touched from the UI thread, which is why
    /// it can be a thread-local (it also keeps parallel tests apart).
    static FEATURE_CLIPBOARD: RefCell<Option<FeatureClipboard>> = const { RefCell::new(None) };
}

/// Whether Copy has put anything on the feature clipboard.
pub(crate) fn clipboard_has_features() -> bool {
    FEATURE_CLIPBOARD.with(|c| c.borrow().as_ref().is_some_and(|clip| !clip.is_empty()))
}

/// A copy of the feature clipboard's content (for tests and inspection).
#[cfg(test)]
pub(crate) fn clipboard_content() -> Option<FeatureClipboard> {
    FEATURE_CLIPBOARD.with(|c| c.borrow().clone())
}

fn set_clipboard(clip: FeatureClipboard) {
    FEATURE_CLIPBOARD.with(|c| *c.borrow_mut() = Some(clip));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClipboardOp {
    Copy,
    Paste,
    PasteAtSource,
    Duplicate,
}

/// The clipboard operation behind the selection action `id`, if it is one.
pub(crate) fn op_for(id: &str) -> Option<ClipboardOp> {
    match id {
        "copy_features" => Some(ClipboardOp::Copy),
        "paste_features" => Some(ClipboardOp::Paste),
        "paste_features_source" => Some(ClipboardOp::PasteAtSource),
        "duplicate_features" => Some(ClipboardOp::Duplicate),
        _ => None,
    }
}

fn is_editable(layers: &LayerManager, id: LayerId) -> bool {
    layers
        .find_layer(id)
        .is_some_and(|layer| layer.as_editable().is_some())
}

/// The layer whose features Copy and Duplicate take: the active layer when
/// it holds part of the selection, else the layer of the first selected
/// feature that has one. `None` if no selected feature is in a data layer.
pub(crate) fn source_layer(
    selection: &[FeatureRef],
    active: Option<LayerId>,
    layers: &LayerManager,
) -> Option<LayerId> {
    if let Some(active) = active {
        if is_editable(layers, active) && selection.iter().any(|f| f.layer_id == active) {
            return Some(active);
        }
    }
    selection
        .iter()
        .map(|f| f.layer_id)
        .find(|&id| is_editable(layers, id))
}

/// Copy and Duplicate apply to a selection with something in a data layer.
pub(crate) fn can_copy(env: &ActionEnv) -> bool {
    source_layer(env.selection, env.active_layer, env.layers).is_some()
}

/// Paste needs something copied and a data layer to paste into.
pub(crate) fn can_paste(env: &ActionEnv) -> bool {
    clipboard_has_features()
        && env
            .active_layer
            .is_some_and(|id| is_editable(env.layers, id))
}

/// The `apply` of the four actions: never called, as
/// `MapViewer::run_selection_action` runs them itself (see the module docs).
pub(crate) fn apply_elsewhere(
    _: &mut LayerManager,
    _: &[FeatureRef],
    _: Option<LayerId>,
) -> Result<ActionOutcome, String> {
    Err(tr!("action.copy_features.hint").to_string())
}

impl MapViewer {
    pub(crate) fn run_clipboard_op(&mut self, op: ClipboardOp, cx: &mut Context<Self>) {
        // Copying changes nothing, so it works during an upload; the rest
        // create objects and are refused like every other edit.
        if op != ClipboardOp::Copy && self.refuse_edit_during_upload() {
            cx.notify();
            return;
        }
        match op {
            ClipboardOp::Copy => self.copy_features(cx),
            ClipboardOp::Paste => {
                let position = self.paste_position();
                self.paste_features(position);
            }
            ClipboardOp::PasteAtSource => self.paste_features(PastePosition::Source),
            ClipboardOp::Duplicate => self.duplicate_features(),
        }
        cx.notify();
    }

    /// Where Paste puts the copied features: centred on the pointer while it
    /// is over the map, else on the middle of the view.
    fn paste_position(&self) -> PastePosition {
        let size = self.viewport.transform.screen_size;
        let at = self
            .last_mouse_pos
            .unwrap_or_else(|| gpui::point(size.width / 2.0, size.height / 2.0));
        let (lat, lon) = self.viewport.screen_to_geo(at);
        PastePosition::CenterAt { lat, lon }
    }

    /// Copy the selection to the feature clipboard (and, when it is not
    /// huge, its OSM XML to the system clipboard).
    fn copy_features(&mut self, cx: &mut Context<Self>) {
        let Some(clip) = self.copy_selection() else {
            self.set_status(tr!("action.copy_features.hint"));
            return;
        };
        if clip.len() <= MAX_OBJECTS_AS_TEXT {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(clip.to_osm_xml()));
        }
        self.set_status(tr!(
            "action.copy_features.done",
            features = tr_count!("action.feature_count", clip.len())
        ));
        set_clipboard(clip);
    }

    /// The selection as a clipboard, taken from its source layer.
    fn copy_selection(&self) -> Option<FeatureClipboard> {
        let layer = source_layer(&self.selected, self.active_layer, &self.layer_manager)?;
        let editable = self.layer_manager.find_layer(layer)?.as_editable()?;
        FeatureClipboard::copy(editable, layer, &self.selected)
    }

    /// Paste the feature clipboard into the active layer at `position`.
    fn paste_features(&mut self, position: PastePosition) {
        let Some(target) = self.active_layer else {
            self.set_status(tr!("action.no_data"));
            return;
        };
        let Some(clip) = FEATURE_CLIPBOARD.with(|c| c.borrow().clone()) else {
            self.set_status(tr!("action.paste_features.hint"));
            return;
        };
        self.paste_clip(&clip, target, position, false);
    }

    /// Copy and paste at the source position in one step, into the layer the
    /// selection is in. The feature clipboard is left as it was.
    fn duplicate_features(&mut self) {
        let Some(layer) = source_layer(&self.selected, self.active_layer, &self.layer_manager)
        else {
            self.set_status(tr!("action.duplicate_features.hint"));
            return;
        };
        let Some(clip) = self.copy_selection() else {
            self.set_status(tr!("action.duplicate_features.hint"));
            return;
        };
        self.paste_clip(&clip, layer, PastePosition::Source, true);
    }

    /// Create `clip`'s features in `target`, record one undo step, select
    /// them and say what happened.
    fn paste_clip(
        &mut self,
        clip: &FeatureClipboard,
        target: LayerId,
        position: PastePosition,
        duplicate: bool,
    ) {
        let limits = Limits::for_server(&settings_store::api_base_url());
        let previous = self.selected.clone();
        let Some(editable) = self
            .layer_manager
            .find_layer_mut(target)
            .and_then(|layer| layer.as_editable_mut())
        else {
            self.set_status(tr!("action.no_data"));
            return;
        };
        match clip.paste(editable, target, position, &limits, &previous) {
            Ok(mut pasted) => {
                pasted.duplicate = duplicate;
                let selection = pasted.selection.clone();
                let features = tr_count!("action.feature_count", pasted.created().len());
                let (done, dropped) = if duplicate {
                    (
                        "action.duplicate_features.done",
                        "action.duplicate_features.done_dropped",
                    )
                } else {
                    (
                        "action.paste_features.done",
                        "action.paste_features.done_dropped",
                    )
                };
                let status = if pasted.dropped_members > 0 {
                    tr_count!(dropped, pasted.dropped_members, features = features)
                } else {
                    tr!(done, features = features)
                };
                self.undo_stack
                    .push(UndoableAction::PasteFeatures { pasted });
                self.set_selection(selection);
                self.set_status(status);
            }
            Err(e) => self.set_status(e.message()),
        }
    }
}
