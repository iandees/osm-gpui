//! The app side of Copy, Paste, Paste at Source Position and Duplicate: the
//! feature clipboard, whether each action applies, and running them on the
//! map view. The copying and pasting itself is `osm_gpui::feature_clipboard`.
//!
//! They are entries of `selection_actions::ACTIONS` (so the Edit menu, the
//! context menu and the keyboard shortcuts treat them like any other action),
//! but they run as `Run::Clipboard`, through [`MapViewer::run_clipboard_op`],
//! because they need the pointer position and the system clipboard, which an
//! `ApplyFn` does not get.

use gpui::Context;
use osm_gpui::feature_clipboard::{FeatureClipboard, PastePosition};
use osm_gpui::layers::{LayerId, LayerManager};
use osm_gpui::limits::Limits;
use osm_gpui::selection::FeatureRef;
use osm_gpui::{settings_store, tr, tr_count};

use crate::selection_actions::ActionEnv;
use crate::undo::UndoableAction;
use crate::MapViewer;

/// The most objects whose OSM XML is also put on the system clipboard; a
/// bigger copy stays app-internal, as building the text would stall the UI.
const MAX_OBJECTS_AS_TEXT: usize = 20_000;

/// What Copy and Copy Tags set aside, for Paste and Paste Tags: the app's own
/// clipboards, kept apart from the system pasteboard. They are viewer state
/// (`MapViewer::clipboards`) and reach the actions through `ActionEnv`.
#[derive(Default)]
pub(crate) struct Clipboards {
    /// The tags Copy Tags took, sorted by key.
    pub tags: Vec<(String, String)>,
    /// The features Copy took.
    pub features: Option<FeatureClipboard>,
}

impl Clipboards {
    /// Whether Copy has put anything on the feature clipboard.
    pub(crate) fn has_features(&self) -> bool {
        self.features.as_ref().is_some_and(|clip| !clip.is_empty())
    }

    /// Replace the tag clipboard with `tags`, sorted by key.
    pub(crate) fn set_tags(&mut self, mut tags: Vec<(String, String)>) {
        tags.sort();
        self.tags = tags;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClipboardOp {
    Copy,
    Paste,
    PasteAtSource,
    Duplicate,
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
    env.clipboards.has_features()
        && env
            .active_layer
            .is_some_and(|id| is_editable(env.layers, id))
}

impl MapViewer {
    pub(crate) fn run_clipboard_op(&mut self, op: ClipboardOp, cx: &mut Context<Self>) {
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
            .pointer
            .last_mouse_pos
            .unwrap_or_else(|| gpui::point(size.width / 2.0, size.height / 2.0));
        PastePosition::CenterAt(self.viewport.screen_to_geo(at))
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
        self.clipboards.features = Some(clip);
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
        let Some(clip) = self.clipboards.features.clone() else {
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
