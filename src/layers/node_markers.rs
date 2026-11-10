//! Which node markers are shown at which zoom, and in which form.
//!
//! Drawing a marker costs about 1.3 us (one `paint_quad` each), so a dense
//! download viewed whole would spend most of a frame on dots too small and
//! too close together to read or click. The rule is by zoom and tags only:
//!
//! * Untagged nodes (plain way vertices) are not shown below
//!   `UNTAGGED_MIN_ZOOM`; from there they are drawn in full.
//! * Individually tagged nodes are always shown. From `UNTAGGED_MIN_ZOOM`
//!   they are drawn in full too; below it as a single small dot, and below
//!   `SIMPLE_SMALL_ZOOM` as a tinier one.
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
/// Below this zoom the simple form of a tagged node is `SIMPLE_TINY_SIZE`
/// rather than `SIMPLE_SIZE`.
pub const SIMPLE_SMALL_ZOOM: f64 = 16.0;
/// Edge of the simple tagged-node dot, in pixels.
pub const SIMPLE_SIZE: f32 = 3.0;
/// Edge of the simple dot at the lowest zooms.
pub const SIMPLE_TINY_SIZE: f32 = 2.0;
/// Colour of the simple dot: a saturated orange that reads over light and
/// dark imagery and over the grey and pastel map styles alike.
pub const SIMPLE_COLOR: u32 = 0xFF6A00;
/// Hit radius, in pixels, of a full marker and of a simple one.
pub const FULL_HIT_RADIUS: f32 = 8.0;
pub const SIMPLE_HIT_RADIUS: f32 = 5.0;

/// How a node's marker is drawn at a given view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NodeForm {
    /// No marker; not a hover or click target.
    Hidden,
    /// One filled dot of this edge length, no casing.
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
    } else if !tagged {
        NodeForm::Hidden
    } else if zoom >= SIMPLE_SMALL_ZOOM {
        NodeForm::Simple(SIMPLE_SIZE)
    } else {
        NodeForm::Simple(SIMPLE_TINY_SIZE)
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
    fn tagged_always_shown_simple_below_and_full_above() {
        assert_eq!(
            node_form(true, false, 12.0),
            NodeForm::Simple(SIMPLE_TINY_SIZE)
        );
        assert_eq!(
            node_form(true, false, SIMPLE_SMALL_ZOOM - 0.01),
            NodeForm::Simple(SIMPLE_TINY_SIZE)
        );
        assert_eq!(
            node_form(true, false, SIMPLE_SMALL_ZOOM),
            NodeForm::Simple(SIMPLE_SIZE)
        );
        assert_eq!(
            node_form(true, false, UNTAGGED_MIN_ZOOM - 0.01),
            NodeForm::Simple(SIMPLE_SIZE)
        );
        assert_eq!(node_form(true, false, UNTAGGED_MIN_ZOOM), NodeForm::Full);
    }

    #[test]
    fn pinned_nodes_are_full_at_any_zoom() {
        for z in [3.0, 14.0, 17.0] {
            assert_eq!(node_form(false, true, z), NodeForm::Full);
            assert_eq!(node_form(true, true, z), NodeForm::Full);
        }
    }

    #[test]
    fn simple_form_is_targetable_hidden_is_not() {
        let simple = node_form(true, false, 14.0);
        assert!(simple.is_shown());
        assert!(simple.hit_radius() >= 4.0);
        let hidden = node_form(false, false, 14.0);
        assert!(!hidden.is_shown());
        assert_eq!(hidden.hit_radius(), 0.0);
        assert!(NodeForm::Full.hit_radius() > simple.hit_radius());
    }
}
