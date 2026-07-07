//! Pure mouse-interaction state machine for the map view.
//!
//! `MapViewer` used to track click-vs-drag-vs-box-select-vs-move-drag with
//! three independent `Option` fields (`mouse_down_pos`, `box_select`,
//! `move_drag`), which made illegal combinations representable (e.g. a
//! box-select *and* a move-drag both active) and made the decision tree
//! untestable, since every handler took gpui event/context types.
//!
//! [`Interaction`] replaces all three with a single enum, and the free
//! functions in this module implement the same decision tree as a set of
//! pure transitions over plain `(f32, f32)` points. `MapViewer`'s gpui
//! handlers become thin adapters: convert event coordinates, call these
//! functions, and act on the result (hit-test, mutate layers, push undo,
//! `cx.notify()`).
//!
//! This is a *behavior-preserving* port of the original branchy handlers —
//! not a redesign — including quirks like the drag threshold being `>=` in
//! some comparisons and `<` in others, and the fact that the "mouse down"
//! position is shared between the left and right mouse buttons (see
//! [`record_mouse_down`]).

use crate::layers::LayerId;

/// A screen-space point as a plain pair, mirroring `gpui::Point<Pixels>`
/// after reducing each axis to `f32` — kept free of gpui types so this
/// module can be unit tested without a gpui context.
pub type Pt = (f32, f32);

/// Minimum screen-pixel distance between a mouse-down and the current
/// position before a press is treated as a drag rather than a click. Was a
/// magic `4.0` scattered across `handle_mouse_move`/`handle_mouse_up`.
pub const DRAG_THRESHOLD: f32 = 4.0;

fn magnitude(a: Pt, b: Pt) -> f32 {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    (dx * dx + dy * dy).sqrt()
}

/// Per-layer node ids being dragged, each with its pre-drag `(lat, lon)` —
/// the shape `resolve_move_targets` produces and a move-drag commit
/// consumes.
pub type NodeMoveTargets = Vec<(LayerId, Vec<(i64, f64, f64)>)>;

/// The map view's current mouse interaction.
#[derive(Debug, Clone, PartialEq)]
pub enum Interaction {
    /// No button is down, or its up event was already fully handled.
    Idle,
    /// A button went down at `down`. Not yet classified as a click or the
    /// start of a box-select drag. This is also the state a right-button
    /// press leaves behind (the right button never progresses past it —
    /// see `handle_mouse_down` in `main.rs`, which only records the
    /// position for the viewport's own pan tracking).
    Pending { down: Pt },
    /// An in-progress left-drag box-select: `down` is the original
    /// mouse-down point, `current` the latest position. Rendered as the
    /// selection-rect overlay.
    BoxSelect { down: Pt, current: Pt },
    /// An in-progress drag of the current selection: `down` is the
    /// mouse-down point, `targets` the resolved per-layer nodes being
    /// moved (snapshotted at drag start).
    MoveDrag { down: Pt, targets: NodeMoveTargets },
}

impl Interaction {
    /// The most recent mouse-down point recorded in this state, if any.
    /// Mirrors reading the old standalone `mouse_down_pos` field.
    pub fn down_pos(&self) -> Option<Pt> {
        match self {
            Interaction::Idle => None,
            Interaction::Pending { down }
            | Interaction::BoxSelect { down, .. }
            | Interaction::MoveDrag { down, .. } => Some(*down),
        }
    }

    /// The in-progress box-select rect (start, current), if any — used by
    /// the render pass to draw the overlay.
    pub fn box_select_rect(&self) -> Option<(Pt, Pt)> {
        match self {
            Interaction::BoxSelect { down, current } => Some((*down, *current)),
            _ => None,
        }
    }
}

/// Record a mouse-down point, mirroring the old unconditional
/// `self.mouse_down_pos = Some(position)` write that both the right-button
/// pan handler and the left-button handler performed *before* any
/// hit-testing. It overwrites only the "down" component of whatever state
/// is active, leaving an in-progress box-select's `current` or a
/// move-drag's `targets` untouched — exactly as the original's standalone
/// `mouse_down_pos` field was independent of `box_select`/`move_drag`.
///
/// This is also what lets a right-button press stomp the "start" point of
/// an unrelated, still-active left-button drag, which the original code
/// did too (whether intentionally or not) since `mouse_down_pos` was one
/// shared field written by both buttons' down handlers.
pub fn record_mouse_down(interaction: &Interaction, pos: Pt) -> Interaction {
    match interaction {
        Interaction::BoxSelect { current, .. } => Interaction::BoxSelect { down: pos, current: *current },
        Interaction::MoveDrag { targets, .. } => Interaction::MoveDrag { down: pos, targets: targets.clone() },
        Interaction::Idle | Interaction::Pending { .. } => Interaction::Pending { down: pos },
    }
}

/// Left-button mouse-down: combines the always-happens `mouse_down_pos`
/// write with the conditional move-drag start. `hit_move_targets` is the
/// result of hit-testing the press against the current selection and
/// resolving it via `resolve_move_targets` (both impure, done by the
/// caller) — `Some` only when non-empty, matching the original's
/// `if !per_layer.is_empty()` guard.
pub fn on_left_mouse_down(pos: Pt, hit_move_targets: Option<NodeMoveTargets>) -> Interaction {
    match hit_move_targets {
        Some(targets) => Interaction::MoveDrag { down: pos, targets },
        None => Interaction::Pending { down: pos },
    }
}

/// Mirrors the early-return branch of the original `handle_mouse_move` that
/// ran whenever a move-drag was active: `Some(delta)` (screen-pixel offset
/// from the drag's start) if the left button is still held, `None` if it
/// isn't — in which case, per the original, nothing happens at all (no
/// viewport pan, no box-select check).
pub fn move_drag_delta(down: Pt, pos: Pt, left_pressed: bool) -> Option<Pt> {
    if left_pressed {
        Some((pos.0 - down.0, pos.1 - down.1))
    } else {
        None
    }
}

/// Mirrors the box-select branch of `handle_mouse_move`, run only when no
/// move-drag is active. Updates `interaction` in place and reports whether
/// the box-select rect changed (so the caller knows to `cx.notify()`).
pub fn update_box_select(interaction: &mut Interaction, pos: Pt, left_pressed: bool) -> bool {
    if !left_pressed {
        return false;
    }
    let Some(down) = interaction.down_pos() else {
        return false;
    };
    let already_boxing = matches!(interaction, Interaction::BoxSelect { .. });
    if magnitude(down, pos) >= DRAG_THRESHOLD || already_boxing {
        *interaction = Interaction::BoxSelect { down, current: pos };
        true
    } else {
        false
    }
}

/// What a left-button mouse-up resolved to.
#[derive(Debug, Clone, PartialEq)]
pub enum Gesture {
    /// Below the drag threshold: a plain click on the map.
    Click { at: Pt },
    /// A move-drag was released past the threshold: apply this
    /// screen-pixel delta to `targets` and commit + push undo.
    MoveCommitted { targets: NodeMoveTargets, delta: Pt },
    /// A move-drag was released at/under the threshold: not actually a
    /// drag, so treat it as a plain click instead (the original's
    /// `if !moved { ... handle_map_click ...; return; }`).
    MoveCancelledAsClick { targets: NodeMoveTargets, at: Pt },
    /// A box-select was released: hit-test this (start, current) rect.
    BoxSelected { rect: (Pt, Pt) },
    /// Nothing tracked (interaction was already `Idle`, or a `Pending`
    /// press that moved past the threshold without ever becoming a
    /// box-select — e.g. a right-button-only press).
    None,
}

/// Resolve a left-button mouse-up against the current interaction, and
/// reset it to `Idle` — mirroring the original's `.take()` calls on all
/// three fields at the top of `handle_mouse_up`.
pub fn on_mouse_up(interaction: &mut Interaction, up_pos: Pt) -> Gesture {
    let taken = std::mem::replace(interaction, Interaction::Idle);
    match taken {
        Interaction::MoveDrag { down, targets } => {
            let delta = (up_pos.0 - down.0, up_pos.1 - down.1);
            if magnitude(down, up_pos) >= DRAG_THRESHOLD {
                Gesture::MoveCommitted { targets, delta }
            } else {
                Gesture::MoveCancelledAsClick { targets, at: up_pos }
            }
        }
        Interaction::BoxSelect { down, .. } => Gesture::BoxSelected { rect: (down, up_pos) },
        Interaction::Pending { down } => {
            if magnitude(down, up_pos) < DRAG_THRESHOLD {
                Gesture::Click { at: up_pos }
            } else {
                Gesture::None
            }
        }
        Interaction::Idle => Gesture::None,
    }
}

/// Cancel an in-progress move-drag (the Escape-key path), returning the
/// targets whose drag preview the caller must clear. `None` (and no state
/// change) if no move-drag was active.
pub fn cancel_move_drag(interaction: &mut Interaction) -> Option<NodeMoveTargets> {
    if let Interaction::MoveDrag { targets, .. } = interaction {
        let targets = std::mem::take(targets);
        *interaction = Interaction::Idle;
        Some(targets)
    } else {
        None
    }
}

/// A normalized axis-aligned rect: non-negative size, top-left origin,
/// regardless of which corner `a`/`b` represent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Normalize two arbitrary screen points into a rect with a top-left origin
/// and non-negative size, regardless of drag direction.
pub fn normalize_rect(a: Pt, b: Pt) -> Rect {
    let min_x = a.0.min(b.0);
    let max_x = a.0.max(b.0);
    let min_y = a.1.min(b.1);
    let max_y = a.1.max(b.1);
    Rect {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    }
}

/// Deduplicate a sequence of (attribution text, optional link URL) pairs by
/// text, keeping the first occurrence's URL and original order — e.g. two
/// visible layers sharing an "© OpenStreetMap contributors" credit collapse
/// to one entry.
pub fn dedupe_attributions(
    items: impl IntoIterator<Item = (String, Option<String>)>,
) -> Vec<(String, Option<String>)> {
    let mut seen = std::collections::HashSet::new();
    let mut credits = Vec::new();
    for (text, url) in items {
        if seen.insert(text.clone()) {
            credits.push((text, url));
        }
    }
    credits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets(layer: u64, ids: &[i64]) -> NodeMoveTargets {
        vec![(
            LayerId(layer),
            ids.iter().map(|&id| (id, 0.0, 0.0)).collect(),
        )]
    }

    // -- click vs. drag threshold ------------------------------------

    #[test]
    fn plain_press_release_below_threshold_is_a_click() {
        let mut interaction = on_left_mouse_down((10.0, 10.0), None);
        assert_eq!(interaction, Interaction::Pending { down: (10.0, 10.0) });

        let gesture = on_mouse_up(&mut interaction, (12.0, 10.0)); // dist 2.0 < 4.0
        assert_eq!(gesture, Gesture::Click { at: (12.0, 10.0) });
        assert_eq!(interaction, Interaction::Idle);
    }

    #[test]
    fn press_release_exactly_at_threshold_is_not_a_click() {
        let mut interaction = on_left_mouse_down((0.0, 0.0), None);
        // magnitude exactly 4.0: original used `< 4.0` for was_click, so
        // exactly-at-threshold does NOT count as a click.
        let gesture = on_mouse_up(&mut interaction, (4.0, 0.0));
        assert_eq!(gesture, Gesture::None);
    }

    #[test]
    fn press_move_past_threshold_without_release_starts_box_select() {
        let mut interaction = on_left_mouse_down((0.0, 0.0), None);
        // Below threshold: no box-select yet.
        assert!(!update_box_select(&mut interaction, (1.0, 0.0), true));
        assert!(matches!(interaction, Interaction::Pending { .. }));

        // At/above threshold: box-select starts.
        assert!(update_box_select(&mut interaction, (5.0, 0.0), true));
        assert_eq!(
            interaction,
            Interaction::BoxSelect { down: (0.0, 0.0), current: (5.0, 0.0) }
        );
    }

    #[test]
    fn box_select_move_without_left_pressed_does_nothing() {
        let mut interaction = on_left_mouse_down((0.0, 0.0), None);
        assert!(!update_box_select(&mut interaction, (100.0, 100.0), false));
        assert!(matches!(interaction, Interaction::Pending { .. }));
    }

    // -- box-select lifecycle -----------------------------------------

    #[test]
    fn box_select_updates_every_move_once_started_even_below_threshold() {
        let mut interaction = Interaction::BoxSelect { down: (0.0, 0.0), current: (5.0, 0.0) };
        // Once box-selecting, `already_boxing` keeps updating regardless of
        // per-move distance from `down`.
        assert!(update_box_select(&mut interaction, (0.5, 0.0), true));
        assert_eq!(
            interaction,
            Interaction::BoxSelect { down: (0.0, 0.0), current: (0.5, 0.0) }
        );
    }

    #[test]
    fn box_select_completes_on_mouse_up() {
        let mut interaction = Interaction::BoxSelect { down: (1.0, 2.0), current: (9.0, 9.0) };
        let gesture = on_mouse_up(&mut interaction, (20.0, 30.0));
        assert_eq!(gesture, Gesture::BoxSelected { rect: ((1.0, 2.0), (20.0, 30.0)) });
        assert_eq!(interaction, Interaction::Idle);
    }

    #[test]
    fn box_select_rect_reads_current_state_for_overlay() {
        let interaction = Interaction::BoxSelect { down: (1.0, 2.0), current: (3.0, 4.0) };
        assert_eq!(interaction.box_select_rect(), Some(((1.0, 2.0), (3.0, 4.0))));
        assert_eq!(Interaction::Idle.box_select_rect(), None);
    }

    // -- move-drag lifecycle --------------------------------------------

    #[test]
    fn move_drag_starts_on_hit_and_tracks_delta_while_held() {
        let t = targets(1, &[10, 11]);
        let interaction = on_left_mouse_down((5.0, 5.0), Some(t.clone()));
        assert_eq!(interaction, Interaction::MoveDrag { down: (5.0, 5.0), targets: t });

        let delta = move_drag_delta((5.0, 5.0), (8.0, 6.0), true);
        assert_eq!(delta, Some((3.0, 1.0)));
    }

    #[test]
    fn move_drag_delta_is_none_and_thus_a_no_op_when_left_released() {
        // Mirrors the original: while move_drag is Some but the left button
        // isn't held, the whole move-handler does nothing (no viewport pan
        // either), which is why the caller must skip that too.
        assert_eq!(move_drag_delta((0.0, 0.0), (100.0, 100.0), false), None);
    }

    #[test]
    fn move_drag_commits_past_threshold() {
        let t = targets(2, &[1]);
        let mut interaction = Interaction::MoveDrag { down: (0.0, 0.0), targets: t.clone() };
        let gesture = on_mouse_up(&mut interaction, (10.0, 0.0));
        assert_eq!(gesture, Gesture::MoveCommitted { targets: t, delta: (10.0, 0.0) });
        assert_eq!(interaction, Interaction::Idle);
    }

    #[test]
    fn move_drag_under_threshold_cancels_as_click() {
        let t = targets(2, &[1]);
        let mut interaction = Interaction::MoveDrag { down: (0.0, 0.0), targets: t.clone() };
        let gesture = on_mouse_up(&mut interaction, (1.0, 0.0)); // magnitude 1.0 < 4.0
        assert_eq!(gesture, Gesture::MoveCancelledAsClick { targets: t, at: (1.0, 0.0) });
        assert_eq!(interaction, Interaction::Idle);
    }

    #[test]
    fn move_drag_exactly_at_threshold_commits_not_cancels() {
        // Original used `>= 4.0` for the move-drag "moved" check (distinct
        // from the plain-click `< 4.0`), so exactly-at-threshold commits.
        let t = targets(2, &[1]);
        let mut interaction = Interaction::MoveDrag { down: (0.0, 0.0), targets: t.clone() };
        let gesture = on_mouse_up(&mut interaction, (4.0, 0.0));
        assert_eq!(gesture, Gesture::MoveCommitted { targets: t, delta: (4.0, 0.0) });
    }

    #[test]
    fn cancel_move_drag_clears_state_and_returns_targets() {
        let t = targets(3, &[7, 8]);
        let mut interaction = Interaction::MoveDrag { down: (0.0, 0.0), targets: t.clone() };
        let cancelled = cancel_move_drag(&mut interaction);
        assert_eq!(cancelled, Some(t));
        assert_eq!(interaction, Interaction::Idle);
    }

    #[test]
    fn cancel_move_drag_is_a_no_op_outside_a_move_drag() {
        let mut interaction = Interaction::BoxSelect { down: (0.0, 0.0), current: (1.0, 1.0) };
        assert_eq!(cancel_move_drag(&mut interaction), None);
        // Unaffected: cancel only acts on MoveDrag.
        assert!(matches!(interaction, Interaction::BoxSelect { .. }));
    }

    // -- shared mouse-down position (right + left buttons) ---------------

    #[test]
    fn record_mouse_down_from_idle_becomes_pending() {
        let interaction = record_mouse_down(&Interaction::Idle, (3.0, 4.0));
        assert_eq!(interaction, Interaction::Pending { down: (3.0, 4.0) });
    }

    #[test]
    fn record_mouse_down_overwrites_down_but_preserves_box_select_current() {
        let interaction = Interaction::BoxSelect { down: (1.0, 1.0), current: (9.0, 9.0) };
        let updated = record_mouse_down(&interaction, (2.0, 2.0));
        assert_eq!(updated, Interaction::BoxSelect { down: (2.0, 2.0), current: (9.0, 9.0) });
    }

    #[test]
    fn record_mouse_down_overwrites_down_but_preserves_move_drag_targets() {
        let t = targets(1, &[1]);
        let interaction = Interaction::MoveDrag { down: (1.0, 1.0), targets: t.clone() };
        let updated = record_mouse_down(&interaction, (2.0, 2.0));
        assert_eq!(updated, Interaction::MoveDrag { down: (2.0, 2.0), targets: t });
    }

    // -- normalize_rect ---------------------------------------------------

    #[test]
    fn normalize_rect_handles_any_drag_direction() {
        assert_eq!(
            normalize_rect((10.0, 20.0), (2.0, 5.0)),
            Rect { x: 2.0, y: 5.0, width: 8.0, height: 15.0 }
        );
        assert_eq!(
            normalize_rect((2.0, 5.0), (10.0, 20.0)),
            Rect { x: 2.0, y: 5.0, width: 8.0, height: 15.0 }
        );
    }

    #[test]
    fn normalize_rect_zero_size_at_a_point() {
        assert_eq!(
            normalize_rect((5.0, 5.0), (5.0, 5.0)),
            Rect { x: 5.0, y: 5.0, width: 0.0, height: 0.0 }
        );
    }

    // -- attribution dedup -------------------------------------------------

    #[test]
    fn dedupe_attributions_keeps_first_occurrence_and_order() {
        let items = vec![
            ("OSM Carto".to_string(), Some("https://osm.org".to_string())),
            ("Imagery Inc".to_string(), None),
            ("OSM Carto".to_string(), Some("https://different.example".to_string())),
        ];
        let credits = dedupe_attributions(items);
        assert_eq!(
            credits,
            vec![
                ("OSM Carto".to_string(), Some("https://osm.org".to_string())),
                ("Imagery Inc".to_string(), None),
            ]
        );
    }

    #[test]
    fn dedupe_attributions_empty_is_empty() {
        assert!(dedupe_attributions(Vec::new()).is_empty());
    }
}
