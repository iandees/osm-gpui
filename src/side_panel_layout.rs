//! The side panel's vertical sizing rules, as pure functions.
//!
//! Top to bottom the panel holds Layers, the middle zone (a title strip, then
//! the Selection or an open note), optional To-do and Validation sections,
//! and History.
//!
//! - The middle zone is the flexible one: it takes whatever the others leave
//!   and scrolls inside it, so nothing in it is ever unreachable.
//! - Layers keeps its natural height up to a share of the panel, then its
//!   list scrolls.
//! - To-do, Validation and History keep a header that is always visible; their lists
//!   keep their natural height up to a share of the panel, then scroll. When
//!   the panel is short they give up height before the middle zone drops
//!   below [`MIDDLE_MIN`]: History first, then To-do, then Validation, each down to
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

const TODO_SHARE: f32 = 0.25;
const TODO_MIN: f32 = 84.0;
const TODO_MAX: f32 = 240.0;

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
    /// `None` when there is no To-do section.
    pub todo: Option<f32>,
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

/// The tallest the To-do list may be in a panel `panel_height` high.
pub(crate) fn todo_cap(panel_height: f32) -> f32 {
    (panel_height * TODO_SHARE).clamp(TODO_MIN, TODO_MAX)
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
    todo: Option<Want>,
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
    // The lists that give up height in a short panel, in the order they do:
    // History first, then To-do, then Validation. (want, cap) each; `None`
    // for a section that is not shown.
    let caps = [
        history_cap(panel_height),
        todo_cap(panel_height),
        validation_cap(panel_height),
    ];
    let wants = [Some(history), todo, validation];
    let desired: Vec<f32> = wants
        .iter()
        .zip(caps)
        .map(|(w, cap)| w.map_or(0.0, |w| wanted(w, cap)))
        .collect();

    // Everything but the lists and the middle zone's body: the section
    // chrome (headers, padding) and the middle zone's title strip.
    let fixed = section_height(layers.open, layers_body)
        + HEADER_HEIGHT
        + wants
            .iter()
            .flatten()
            .map(|w| section_height(w.open, 0.0))
            .sum::<f32>();
    // What is left for the lists once the middle zone has its minimum.
    let spare = panel_height - fixed - MIDDLE_MIN;

    // Each list in turn takes what the later ones (which shrink after it)
    // leave, down to its floor.
    let mut bodies = [0.0f32; 3];
    for i in 0..3 {
        let later: f32 = desired[i + 1..].iter().sum();
        let earlier: f32 = bodies[..i].iter().sum();
        let floor = desired[i].min(BODY_FLOOR);
        bodies[i] = (spare - later - earlier).clamp(floor, desired[i]);
    }
    // Lists that were given less than they wanted on the first pass may take
    // back what later lists, held to their floors, did not use.
    for i in 0..3 {
        let others: f32 = bodies
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, b)| b)
            .sum();
        bodies[i] = (spare - others).clamp(bodies[i], desired[i]);
    }
    // A list that was not shrunk may use its whole cap (the content estimate
    // only sizes the budget: rows that wrap are taller than estimated); a
    // shrunk one is held to the height it was given.
    let limit = |i: usize| -> Option<f32> {
        let w = wants[i]?;
        Some(if !w.open {
            0.0
        } else if bodies[i] >= desired[i] {
            caps[i]
        } else {
            bodies[i]
        })
    };
    Plan {
        middle: (panel_height - fixed - bodies.iter().sum::<f32>()).max(0.0),
        layers: if layers.open {
            layers_cap(panel_height)
        } else {
            0.0
        },
        history: limit(0).unwrap_or(0.0),
        todo: limit(1),
        validation: limit(2),
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
        let p = plan(1200.0, open(60.0), None, Some(open(100.0)), open(500.0));
        assert_eq!(p.layers, LAYERS_MAX);
        assert_eq!(p.validation, Some(VALIDATION_MAX));
        assert_eq!(p.history, HISTORY_MAX);
    }

    #[test]
    fn no_validation_means_no_validation_height() {
        let p = plan(776.0, open(100.0), None, None, open(30.0));
        assert_eq!(p.validation, None);
        assert_eq!(p.history, history_cap(776.0));
    }

    #[test]
    fn history_shrinks_before_validation() {
        let tall = plan(776.0, open(60.0), None, Some(open(400.0)), open(400.0));
        let short = plan(560.0, open(60.0), None, Some(open(400.0)), open(400.0));
        assert!(short.history < tall.history);
        assert!(short.history <= short.validation.unwrap());
        // Where there is just enough, validation keeps its full cap while
        // history takes what remains.
        let cap = validation_cap(620.0);
        let mid = plan(620.0, open(60.0), None, Some(open(400.0)), open(400.0));
        assert!(mid.validation.unwrap() <= cap);
        assert!(mid.history >= BODY_FLOOR);
    }

    #[test]
    fn lists_never_go_below_their_floor_while_open() {
        let p = plan(300.0, open(500.0), None, Some(open(500.0)), open(500.0));
        assert_eq!(p.history, BODY_FLOOR);
        assert_eq!(p.validation, Some(BODY_FLOOR));
    }

    #[test]
    fn a_short_list_is_not_shrunk_below_what_it_needs() {
        // 800 high: plenty of room, so the caps apply, not the floors.
        let p = plan(800.0, open(10.0), None, Some(open(20.0)), open(24.0));
        assert_eq!(p.validation, Some(validation_cap(800.0)));
        assert_eq!(p.history, history_cap(800.0));
        // A shrunk list is held to what it was given.
        let tight = plan(400.0, open(10.0), None, Some(open(200.0)), open(200.0));
        assert_eq!(tight.history, BODY_FLOOR);
    }

    #[test]
    fn collapsing_frees_space_for_the_others() {
        let all = plan(520.0, open(100.0), None, Some(open(300.0)), open(300.0));
        let no_history = plan(520.0, open(100.0), None, Some(open(300.0)), closed(300.0));
        assert_eq!(no_history.history, 0.0);
        assert!(no_history.validation.unwrap() >= all.validation.unwrap());
        let no_validation = plan(520.0, open(100.0), None, Some(closed(300.0)), open(300.0));
        assert_eq!(no_validation.validation, Some(0.0));
        assert!(no_validation.history >= all.history);
    }

    #[test]
    fn by_default_the_middle_zone_gets_most_of_the_panel() {
        // At start-up: Layers open, History collapsed, no Validation.
        let p = plan(776.0, open(60.0), None, None, closed(240.0));
        assert!(p.middle > 776.0 / 2.0, "{}", p.middle);
        // After Validate (collapsed too): the same.
        let v = plan(776.0, open(60.0), None, Some(closed(600.0)), closed(240.0));
        assert!(v.middle > 776.0 / 2.0, "{}", v.middle);
        assert!(v.pins_selection_header());
    }

    #[test]
    fn the_selection_header_is_pinned_only_when_there_is_room() {
        // Nothing else open: lots of room.
        let roomy = plan(776.0, closed(0.0), None, None, closed(0.0));
        assert!(roomy.pins_selection_header());
        // Everything open in a short window: the header scrolls instead.
        let tight = plan(496.0, open(500.0), None, Some(open(500.0)), open(500.0));
        assert!(!tight.pins_selection_header());
        assert!(tight.middle > 0.0);
    }

    #[test]
    fn the_middle_zone_keeps_its_minimum_when_the_lists_can_give() {
        for height in [456.0_f32, 520.0, 600.0, 776.0, 1000.0] {
            let p = plan(height, open(500.0), None, Some(open(500.0)), open(500.0));
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

    #[test]
    fn history_shrinks_before_todo_and_todo_before_validation() {
        let p = plan(
            560.0,
            open(60.0),
            Some(open(400.0)),
            Some(open(400.0)),
            open(400.0),
        );
        let (todo, validation) = (p.todo.unwrap(), p.validation.unwrap());
        assert!(p.history <= todo, "{p:?}");
        assert!(todo <= validation, "{p:?}");
        let floors = plan(
            300.0,
            open(500.0),
            Some(open(500.0)),
            Some(open(500.0)),
            open(500.0),
        );
        assert_eq!(floors.todo, Some(BODY_FLOOR));
    }

    #[test]
    fn no_todo_means_no_todo_height() {
        let p = plan(776.0, open(100.0), None, None, open(30.0));
        assert_eq!(p.todo, None);
        let roomy = plan(1200.0, open(60.0), Some(open(100.0)), None, open(500.0));
        assert_eq!(roomy.todo, Some(todo_cap(1200.0)));
    }
}
