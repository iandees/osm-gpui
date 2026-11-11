//! The side panel's vertical sizing rules, as pure functions.
//!
//! Top to bottom the panel holds Layers, the middle zone (a title strip, then
//! the Selection or an open note), an optional Validation section and History.
//!
//! - The middle zone is the flexible one: it takes whatever the others leave
//!   and scrolls inside it, so nothing in it is ever unreachable.
//! - Layers keeps its natural height up to a share of the panel, then its
//!   list scrolls.
//! - Validation and History keep a header that is always visible; their lists
//!   keep their natural height up to a share of the panel, then scroll. When
//!   the panel is short they give up height before the middle zone drops
//!   below [`MIDDLE_MIN`]: History first, then Validation, each down to
//!   [`BODY_FLOOR`] (about a row and a half). Below that the middle zone
//!   gives way (it still scrolls).

/// The height of a section header and of the middle zone's title strip.
pub(crate) const HEADER_HEIGHT: f32 = 32.0;
/// The padding around an open section's body (above and below).
pub(crate) const BODY_PADDING: f32 = 10.0;
/// What the middle zone's body (below its strip) keeps while the others can
/// still shrink.
pub(crate) const MIDDLE_MIN: f32 = 180.0;
/// The least an open Validation or History list shrinks to.
pub(crate) const BODY_FLOOR: f32 = 40.0;

const LAYERS_SHARE: f32 = 0.22;
const LAYERS_MIN: f32 = 64.0;
const LAYERS_MAX: f32 = 160.0;

const VALIDATION_SHARE: f32 = 0.28;
const VALIDATION_MIN: f32 = 84.0;
const VALIDATION_MAX: f32 = 260.0;

const HISTORY_SHARE: f32 = 0.25;
const HISTORY_MIN: f32 = 56.0;
const HISTORY_MAX: f32 = 200.0;

/// What a section asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Want {
    pub open: bool,
    /// The height its list would have with room to spare.
    pub content: f32,
}

/// The tallest each list may be (a `max_h`; zero when its section is closed).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Plan {
    pub layers: f32,
    /// `None` when there is no Validation section.
    pub validation: Option<f32>,
    pub history: f32,
    /// About what the middle zone has below its title strip.
    pub middle: f32,
}

/// The Selection header (icon, name, kind, last edit) stays pinned above the
/// scrolling Fields and Tags only while the middle zone is at least this
/// tall; in a shorter one it scrolls along with them, so they keep room.
pub(crate) const PIN_HEADER_MIN_MIDDLE: f32 = 240.0;

impl Plan {
    pub(crate) fn pins_selection_header(&self) -> bool {
        self.middle >= PIN_HEADER_MIN_MIDDLE
    }
}

/// The tallest the Layers list may be in a panel `panel_height` high.
pub(crate) fn layers_cap(panel_height: f32) -> f32 {
    (panel_height * LAYERS_SHARE).clamp(LAYERS_MIN, LAYERS_MAX)
}

/// The tallest the Validation list may be in a panel `panel_height` high.
pub(crate) fn validation_cap(panel_height: f32) -> f32 {
    (panel_height * VALIDATION_SHARE).clamp(VALIDATION_MIN, VALIDATION_MAX)
}

/// The tallest the History list may be in a panel `panel_height` high.
pub(crate) fn history_cap(panel_height: f32) -> f32 {
    (panel_height * HISTORY_SHARE).clamp(HISTORY_MIN, HISTORY_MAX)
}

/// The height of a section: its header, plus its padded body when open.
fn section_height(open: bool, body: f32) -> f32 {
    HEADER_HEIGHT + if open { BODY_PADDING + body } else { 0.0 }
}

/// How tall each list may be so that, as far as possible, the middle zone
/// keeps [`MIDDLE_MIN`].
pub(crate) fn plan(
    panel_height: f32,
    layers: Want,
    validation: Option<Want>,
    history: Want,
) -> Plan {
    let layers_body = if layers.open {
        // Layers is never shrunk: it is bounded by its own cap.
        layers.content.min(layers_cap(panel_height)).max(0.0)
    } else {
        0.0
    };
    let wanted = |want: Want, cap: f32| {
        if want.open {
            want.content.min(cap).max(0.0)
        } else {
            0.0
        }
    };
    let validation_want = validation.map_or(0.0, |w| wanted(w, validation_cap(panel_height)));
    let history_want = wanted(history, history_cap(panel_height));

    // Everything but the lists and the middle zone's body: the section
    // chrome (headers, padding) and the middle zone's title strip.
    let fixed = section_height(layers.open, layers_body)
        + HEADER_HEIGHT
        + validation.map_or(0.0, |w| section_height(w.open, 0.0))
        + section_height(history.open, 0.0);
    // What is left for the two lists once the middle zone has its minimum.
    let spare = panel_height - fixed - MIDDLE_MIN;

    // History gives up height first, then Validation, each down to its floor.
    let history_floor = history_want.min(BODY_FLOOR);
    let validation_floor = validation_want.min(BODY_FLOOR);
    let history_body = (spare - validation_want).clamp(history_floor, history_want);
    let validation_body = (spare - history_body).clamp(validation_floor, validation_want);
    // A list that was not shrunk may use its whole cap (the content estimate
    // only sizes the budget: rows that wrap are taller than estimated); a
    // shrunk one is held to the height it was given.
    let limit = |body: f32, wanted: f32, cap: f32| if body >= wanted { cap } else { body };
    Plan {
        middle: (panel_height - fixed - validation_body - history_body).max(0.0),
        layers: if layers.open {
            layers_cap(panel_height)
        } else {
            0.0
        },
        validation: validation.map(|w| {
            if w.open {
                limit(
                    validation_body,
                    validation_want,
                    validation_cap(panel_height),
                )
            } else {
                0.0
            }
        }),
        history: if history.open {
            limit(history_body, history_want, history_cap(panel_height))
        } else {
            0.0
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(content: f32) -> Want {
        Want {
            open: true,
            content,
        }
    }

    fn closed(content: f32) -> Want {
        Want {
            open: false,
            content,
        }
    }

    #[test]
    fn caps_follow_the_panel_height_within_bounds() {
        assert_eq!(layers_cap(200.0), LAYERS_MIN);
        assert_eq!(layers_cap(2000.0), LAYERS_MAX);
        assert_eq!(validation_cap(2000.0), VALIDATION_MAX);
        assert_eq!(history_cap(100.0), HISTORY_MIN);
        assert_eq!(history_cap(600.0), 150.0);
    }

    #[test]
    fn a_tall_panel_lets_every_list_use_its_cap() {
        let p = plan(1200.0, open(60.0), Some(open(100.0)), open(500.0));
        assert_eq!(p.layers, LAYERS_MAX);
        assert_eq!(p.validation, Some(VALIDATION_MAX));
        assert_eq!(p.history, HISTORY_MAX);
    }

    #[test]
    fn no_validation_means_no_validation_height() {
        let p = plan(776.0, open(100.0), None, open(30.0));
        assert_eq!(p.validation, None);
        assert_eq!(p.history, history_cap(776.0));
    }

    #[test]
    fn history_shrinks_before_validation() {
        let tall = plan(776.0, open(60.0), Some(open(400.0)), open(400.0));
        let short = plan(560.0, open(60.0), Some(open(400.0)), open(400.0));
        assert!(short.history < tall.history);
        assert!(short.history <= short.validation.unwrap());
        // Where there is just enough, validation keeps its full cap while
        // history takes what remains.
        let cap = validation_cap(620.0);
        let mid = plan(620.0, open(60.0), Some(open(400.0)), open(400.0));
        assert!(mid.validation.unwrap() <= cap);
        assert!(mid.history >= BODY_FLOOR);
    }

    #[test]
    fn lists_never_go_below_their_floor_while_open() {
        let p = plan(300.0, open(500.0), Some(open(500.0)), open(500.0));
        assert_eq!(p.history, BODY_FLOOR);
        assert_eq!(p.validation, Some(BODY_FLOOR));
    }

    #[test]
    fn a_short_list_is_not_shrunk_below_what_it_needs() {
        // 800 high: plenty of room, so the caps apply, not the floors.
        let p = plan(800.0, open(10.0), Some(open(20.0)), open(24.0));
        assert_eq!(p.validation, Some(validation_cap(800.0)));
        assert_eq!(p.history, history_cap(800.0));
        // A shrunk list is held to what it was given.
        let tight = plan(400.0, open(10.0), Some(open(200.0)), open(200.0));
        assert_eq!(tight.history, BODY_FLOOR);
    }

    #[test]
    fn collapsing_frees_space_for_the_others() {
        let all = plan(520.0, open(100.0), Some(open(300.0)), open(300.0));
        let no_history = plan(520.0, open(100.0), Some(open(300.0)), closed(300.0));
        assert_eq!(no_history.history, 0.0);
        assert!(no_history.validation.unwrap() >= all.validation.unwrap());
        let no_validation = plan(520.0, open(100.0), Some(closed(300.0)), open(300.0));
        assert_eq!(no_validation.validation, Some(0.0));
        assert!(no_validation.history >= all.history);
    }

    #[test]
    fn the_selection_header_is_pinned_only_when_there_is_room() {
        // Nothing else open: lots of room.
        let roomy = plan(776.0, closed(0.0), None, closed(0.0));
        assert!(roomy.pins_selection_header());
        // Everything open in a short window: the header scrolls instead.
        let tight = plan(496.0, open(500.0), Some(open(500.0)), open(500.0));
        assert!(!tight.pins_selection_header());
        assert!(tight.middle > 0.0);
    }

    #[test]
    fn the_middle_zone_keeps_its_minimum_when_the_lists_can_give() {
        for height in [456.0_f32, 520.0, 600.0, 776.0, 1000.0] {
            let p = plan(height, open(500.0), Some(open(500.0)), open(500.0));
            let used = section_height(true, p.layers)
                + HEADER_HEIGHT
                + section_height(true, p.validation.unwrap())
                + section_height(true, p.history);
            let middle = height - used;
            let floors = section_height(true, p.layers)
                + HEADER_HEIGHT
                + 2.0 * section_height(true, BODY_FLOOR);
            if height - floors >= MIDDLE_MIN {
                assert!(middle >= MIDDLE_MIN - 0.01, "height {height}: {middle}");
            }
            assert!(middle > 0.0, "height {height}");
        }
    }
}
