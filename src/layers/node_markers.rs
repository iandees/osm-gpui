//! Which node markers are shown at which zoom, and in which form.
//!
//! Drawing a marker costs about 1.3 us (one `paint_quad` each), so a dense
//! download viewed whole would spend most of a frame on dots too small and
//! too close together to read or click. The rule is by zoom and tags only:
//!
//! * Untagged nodes that belong to a way (plain vertices) are never drawn,
//!   at any zoom, unless they are selected or on a selected way.
//! * Individually tagged nodes are not shown below `TAGGED_MIN_ZOOM`: at
//!   that scale a point of interest cannot be picked out or edited, and in a
//!   dense city their dots dominate the map. From `TAGGED_MIN_ZOOM` up to
//!   `FULL_MIN_ZOOM` they are a tiny empty square (outline only); from
//!   `FULL_MIN_ZOOM` they are drawn in full.
//! * An untagged node that belongs to no way ("standalone", e.g. one just
//!   added in Add mode) would otherwise be invisible and unselectable for
//!   good, so it follows the tagged rule.
//!
//! The vertices of the way under the pointer (the "preview way") are shown,
//! small and light, from `PREVIEW_MIN_ZOOM`, and are hover/click/drag
//! targets while it is previewed.
//!
//! Selected nodes, the hovered node and the nodes of selected ways are shown
//! in full at any zoom; that is decided by the layer, which passes
//! `pinned = true` to `node_form`. The one function `node_form` answers
//! drawing, hover, click and box select, so what you can target is what you
//! can see. (Snapping a new point in Add mode is separate: it looks at every
//! node, drawn or not.)

use crate::style::mapcss::NodeVisual;

/// Tagged and standalone nodes are drawn in full from this zoom.
pub const FULL_MIN_ZOOM: f64 = 18.0;
/// Tagged and standalone nodes are shown (as tiny empty squares) from this
/// zoom. Below it only selected, hovered and selected-way nodes are drawn.
pub const TAGGED_MIN_ZOOM: f64 = 17.0;
/// Outer edge of the simple tagged-node square, in logical pixels (8 device
/// pixels on a 2x display).
pub const SIMPLE_SIZE: f32 = 4.0;
/// Stroke width of the simple square, in logical pixels (2 device pixels on
/// a 2x display).
pub const SIMPLE_STROKE: f32 = 1.0;
/// Colour of the simple square: the dark outline colour of the full marker
/// drawn from `FULL_MIN_ZOOM`, so the step to the full marker looks
/// related.
pub const SIMPLE_COLOR: u32 = 0x1C1C1E;
/// `SIMPLE_COLOR` as opaque RGBA.
pub const SIMPLE_RGBA: u32 = (SIMPLE_COLOR << 8) | 0xFF;
/// Hit radius, in pixels, of a full marker and of a simple one.
pub const FULL_HIT_RADIUS: f32 = 8.0;
pub const SIMPLE_HIT_RADIUS: f32 = 5.0;
/// The vertices of the way under the pointer are previewed from this zoom
/// (the one where tagged nodes get their full marker): below it vertices are
/// too close together to grab, and the map would flash as the pointer crosses
/// many ways.
pub const PREVIEW_MIN_ZOOM: f64 = FULL_MIN_ZOOM;
/// Diameter of a previewed vertex: smaller than a selected way's vertex.
pub const PREVIEW_DIAMETER: f32 = 7.0;
/// Hit radius, in pixels, of a previewed vertex.
pub const PREVIEW_HIT_RADIUS: f32 = 7.0;

/// Styled markers (hollow squares and crosses from the stylesheet) are drawn
/// from this zoom.
pub const STYLED_MIN_ZOOM: f64 = 16.0;

/// How a node's marker is drawn at a given view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NodeForm {
    /// No marker; not a hover or click target.
    Hidden,
    /// An empty square of this outer edge length (outline only).
    Simple(f32),
    /// The full marker: casing and the style's colour and size.
    Full,
    /// A vertex of the way under the pointer: a small white dot with a dark
    /// outline, lighter than a selected way's vertices. A hover and click
    /// target.
    Preview,
    /// The stylesheet's own marker (`symbol-shape: square | cross`): drawn
    /// from `VERTEX_MIN_ZOOM` for every node it matches, vertices included.
    Styled(NodeVisual),
}

impl NodeForm {
    pub fn is_shown(self) -> bool {
        self != NodeForm::Hidden
    }

    /// Hover and click radius in pixels.
    pub fn hit_radius(self) -> f32 {
        match self {
            NodeForm::Hidden => 0.0,
            NodeForm::Simple(_) => SIMPLE_HIT_RADIUS,
            NodeForm::Full => FULL_HIT_RADIUS,
            NodeForm::Preview => PREVIEW_HIT_RADIUS,
            NodeForm::Styled(v) => (v.size / 2.0 + 3.0).max(SIMPLE_HIT_RADIUS),
        }
    }
}

/// The form of a node's marker at `zoom`. `pinned` is true for selected
/// nodes, the hovered node and the nodes of selected ways. `standalone` is
/// true for an untagged node that belongs to no way (callers may pass false
/// for tagged nodes, where it makes no difference).
pub fn node_form(tagged: bool, standalone: bool, pinned: bool, zoom: f64) -> NodeForm {
    node_form_previewed(tagged, standalone, pinned, false, zoom)
}

/// Like `node_form`, with `previewed` true for a node of the way under the
/// pointer. A previewed node that would otherwise be hidden is shown in the
/// preview form from `PREVIEW_MIN_ZOOM`; any form it already has wins.
pub fn node_form_previewed(
    tagged: bool,
    standalone: bool,
    pinned: bool,
    previewed: bool,
    zoom: f64,
) -> NodeForm {
    let form = base_form(tagged, standalone, pinned, zoom);
    if form == NodeForm::Hidden && previewed && zoom >= PREVIEW_MIN_ZOOM {
        NodeForm::Preview
    } else {
        form
    }
}

fn base_form(tagged: bool, standalone: bool, pinned: bool, zoom: f64) -> NodeForm {
    if pinned {
        NodeForm::Full
    } else if !(tagged || standalone) {
        NodeForm::Hidden
    } else if zoom >= FULL_MIN_ZOOM {
        NodeForm::Full
    } else if zoom >= TAGGED_MIN_ZOOM {
        NodeForm::Simple(SIMPLE_SIZE)
    } else {
        NodeForm::Hidden
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untagged_vertex_is_never_drawn_at_any_zoom() {
        for z in [3.0, 14.0, 17.0, 17.99, 18.0, 20.0, 22.0] {
            assert_eq!(node_form(false, false, false, z), NodeForm::Hidden, "z{z}");
        }
    }

    #[test]
    fn untagged_vertex_of_a_selected_way_is_full_at_any_zoom() {
        for z in [3.0, 14.0, 17.0, 18.0, 20.0] {
            assert_eq!(node_form(false, false, true, z), NodeForm::Full, "z{z}");
        }
    }

    #[test]
    fn standalone_untagged_node_follows_the_tagged_rule() {
        for z in [3.0, 14.0, 16.99] {
            assert_eq!(node_form(false, true, false, z), NodeForm::Hidden);
        }
        assert_eq!(
            node_form(false, true, false, 17.0),
            NodeForm::Simple(SIMPLE_SIZE)
        );
        assert_eq!(node_form(false, true, false, 18.0), NodeForm::Full);
        assert_eq!(node_form(false, true, false, 20.0), NodeForm::Full);
    }

    #[test]
    fn tagged_hidden_below_17_simple_to_18_and_full_from_18() {
        assert_eq!(node_form(true, false, false, 3.0), NodeForm::Hidden);
        assert_eq!(node_form(true, false, false, 14.0), NodeForm::Hidden);
        assert_eq!(node_form(true, false, false, 16.0), NodeForm::Hidden);
        assert_eq!(node_form(true, false, false, 16.99), NodeForm::Hidden);
        assert_eq!(
            node_form(true, false, false, 17.0),
            NodeForm::Simple(SIMPLE_SIZE)
        );
        assert_eq!(
            node_form(true, false, false, 17.99),
            NodeForm::Simple(SIMPLE_SIZE)
        );
        assert_eq!(node_form(true, false, false, FULL_MIN_ZOOM), NodeForm::Full);
        assert_eq!(node_form(true, false, false, 20.0), NodeForm::Full);
    }

    #[test]
    fn the_thresholds_are_ordered() {
        assert_eq!(TAGGED_MIN_ZOOM, 17.0);
        assert_eq!(FULL_MIN_ZOOM, 18.0);
        const { assert!(TAGGED_MIN_ZOOM < FULL_MIN_ZOOM) };
    }

    #[test]
    fn simple_square_has_room_for_an_empty_middle() {
        // Outline only: the stroke on both sides leaves an inside.
        const { assert!(SIMPLE_SIZE > 2.0 * SIMPLE_STROKE) };
        assert_eq!(SIMPLE_RGBA & 0xFF, 0xFF);
    }

    #[test]
    fn pinned_nodes_are_full_at_any_zoom() {
        for z in [3.0, 14.0, 16.0, 17.0] {
            assert_eq!(node_form(false, false, true, z), NodeForm::Full);
            assert_eq!(node_form(true, false, true, z), NodeForm::Full);
        }
    }

    #[test]
    fn previewed_vertex_is_shown_small_from_the_preview_zoom_only() {
        for z in [3.0, 16.0, 17.0, 17.99] {
            assert_eq!(
                node_form_previewed(false, false, false, true, z),
                NodeForm::Hidden,
                "z{z}"
            );
        }
        for z in [18.0, 19.0, 22.0] {
            assert_eq!(
                node_form_previewed(false, false, false, true, z),
                NodeForm::Preview,
                "z{z}"
            );
        }
        // Not previewed: still hidden.
        assert_eq!(
            node_form_previewed(false, false, false, false, 19.0),
            NodeForm::Hidden
        );
    }

    #[test]
    fn preview_never_downgrades_a_form_the_node_already_has() {
        assert_eq!(
            node_form_previewed(false, false, true, true, 19.0),
            NodeForm::Full
        );
        assert_eq!(
            node_form_previewed(true, false, false, true, 19.0),
            NodeForm::Full
        );
    }

    #[test]
    fn preview_form_is_a_target_and_lighter_than_full() {
        assert!(NodeForm::Preview.is_shown());
        assert!(NodeForm::Preview.hit_radius() >= 5.0);
        assert!(NodeForm::Preview.hit_radius() <= NodeForm::Full.hit_radius());
        assert_eq!(PREVIEW_MIN_ZOOM, FULL_MIN_ZOOM);
    }

    #[test]
    fn simple_form_is_targetable_hidden_is_not() {
        let simple = node_form(true, false, false, 17.0);
        assert!(simple.is_shown());
        assert!(simple.hit_radius() >= 4.0);
        let hidden = node_form(true, false, false, 16.0);
        assert!(!hidden.is_shown());
        assert_eq!(hidden.hit_radius(), 0.0);
        assert!(NodeForm::Full.hit_radius() > simple.hit_radius());
    }
}
