//! Which node markers are shown at which zoom, and in which form.
//!
//! Drawing a marker costs about 1.3 us (one `paint_quad` each), so a dense
//! download viewed whole would spend most of a frame on dots too small and
//! too close together to read or click. The rule is by zoom and tags only:
//!
//! * Untagged nodes (plain way vertices) are not shown below
//!   `UNTAGGED_MIN_ZOOM`; from there they are drawn in full.
//! * Individually tagged nodes are not shown below `TAGGED_MIN_ZOOM`: at
//!   that scale a point of interest cannot be picked out or edited, and in a
//!   dense city their dots dominate the map. From `TAGGED_MIN_ZOOM` up to
//!   `UNTAGGED_MIN_ZOOM` they are a tiny empty square (outline only); from
//!   `UNTAGGED_MIN_ZOOM` they are drawn in full.
//!
//! Selected nodes, the hovered node and the nodes of selected ways are shown
//! in full at any zoom; that is decided by the layer, which passes
//! `pinned = true` to `node_form`. The one function `node_form` answers
//! drawing, hover, click and box select, so what you can target is what you
//! can see.

/// Untagged nodes are shown (and everything is drawn in full) from this
/// zoom. Chosen from measurements on a 24,000-node download: below it the
/// layer paint exceeds the 8 ms budget (see docs/perf), and it is the zoom
/// where individual vertices of a typical street are at least a few pixels
/// apart.
pub const UNTAGGED_MIN_ZOOM: f64 = 18.0;
/// Tagged nodes are shown (as tiny empty squares) from this zoom. Below it
/// only selected, hovered and selected-way nodes are drawn.
pub const TAGGED_MIN_ZOOM: f64 = 17.0;
/// Outer edge of the simple tagged-node square, in logical pixels (8 device
/// pixels on a 2x display).
pub const SIMPLE_SIZE: f32 = 4.0;
/// Stroke width of the simple square, in logical pixels (2 device pixels on
/// a 2x display).
pub const SIMPLE_STROKE: f32 = 1.0;
/// Colour of the simple square: the dark outline colour of the full marker
/// drawn from `UNTAGGED_MIN_ZOOM`, so the step to the full marker looks
/// related.
pub const SIMPLE_COLOR: u32 = 0x1C1C1E;
/// `SIMPLE_COLOR` as opaque RGBA.
pub const SIMPLE_RGBA: u32 = (SIMPLE_COLOR << 8) | 0xFF;
/// Hit radius, in pixels, of a full marker and of a simple one.
pub const FULL_HIT_RADIUS: f32 = 8.0;
pub const SIMPLE_HIT_RADIUS: f32 = 5.0;

/// How a node's marker is drawn at a given view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NodeForm {
    /// No marker; not a hover or click target.
    Hidden,
    /// An empty square of this outer edge length (outline only).
    Simple(f32),
    /// The full marker: casing and the style's colour and size.
    Full,
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
        }
    }
}

/// The form of a node's marker at `zoom`. `pinned` is true for selected
/// nodes, the hovered node and the nodes of selected ways.
pub fn node_form(tagged: bool, pinned: bool, zoom: f64) -> NodeForm {
    if pinned || zoom >= UNTAGGED_MIN_ZOOM {
        NodeForm::Full
    } else if tagged && zoom >= TAGGED_MIN_ZOOM {
        NodeForm::Simple(SIMPLE_SIZE)
    } else {
        NodeForm::Hidden
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untagged_hidden_below_the_threshold_and_full_from_it() {
        assert_eq!(node_form(false, false, 14.0), NodeForm::Hidden);
        assert_eq!(node_form(false, false, 17.99), NodeForm::Hidden);
        assert_eq!(node_form(false, false, UNTAGGED_MIN_ZOOM), NodeForm::Full);
        assert_eq!(node_form(false, false, 20.0), NodeForm::Full);
    }

    #[test]
    fn tagged_hidden_below_17_simple_to_18_and_full_from_18() {
        assert_eq!(node_form(true, false, 3.0), NodeForm::Hidden);
        assert_eq!(node_form(true, false, 14.0), NodeForm::Hidden);
        assert_eq!(node_form(true, false, 16.0), NodeForm::Hidden);
        assert_eq!(node_form(true, false, 16.99), NodeForm::Hidden);
        assert_eq!(node_form(true, false, 17.0), NodeForm::Simple(SIMPLE_SIZE));
        assert_eq!(node_form(true, false, 17.99), NodeForm::Simple(SIMPLE_SIZE));
        assert_eq!(node_form(true, false, UNTAGGED_MIN_ZOOM), NodeForm::Full);
        assert_eq!(node_form(true, false, 20.0), NodeForm::Full);
    }

    #[test]
    fn the_thresholds_are_ordered() {
        assert_eq!(TAGGED_MIN_ZOOM, 17.0);
        const { assert!(TAGGED_MIN_ZOOM < UNTAGGED_MIN_ZOOM) };
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
            assert_eq!(node_form(false, true, z), NodeForm::Full);
            assert_eq!(node_form(true, true, z), NodeForm::Full);
        }
    }

    #[test]
    fn simple_form_is_targetable_hidden_is_not() {
        let simple = node_form(true, false, 17.0);
        assert!(simple.is_shown());
        assert!(simple.hit_radius() >= 4.0);
        let hidden = node_form(true, false, 16.0);
        assert!(!hidden.is_shown());
        assert_eq!(hidden.hit_radius(), 0.0);
        assert!(NodeForm::Full.hit_radius() > simple.hit_radius());
    }
}
