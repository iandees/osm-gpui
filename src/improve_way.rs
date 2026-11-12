//! Improve Way mode's geometry: given a way's vertices (in screen pixels) and
//! the cursor, decide which edit a click would make and what the way would
//! look like afterwards.
//!
//! The plain click moves the vertex nearest the cursor onto the cursor; with
//! the "add" modifier it inserts a new vertex at the point of the way nearest
//! the cursor instead; with Shift it deletes the nearest vertex.

pub type Pt = (f64, f64);

/// The kind of edit a click makes, chosen by the held modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Move,
    Insert,
    Delete,
}

impl Op {
    /// Shift deletes, the add modifier (Ctrl/Cmd/Alt) inserts, none moves.
    pub fn from_modifiers(shift: bool, add: bool) -> Op {
        if shift {
            Op::Delete
        } else if add {
            Op::Insert
        } else {
            Op::Move
        }
    }
}

/// A concrete edit of one way. Positions are in the same space as the points
/// the plan was made from (screen pixels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Plan {
    /// Move node `node` to `to`.
    Move { node: i64, to: Pt },
    /// Insert a new vertex at `at` so that it ends up at `index` in the way.
    Insert { index: usize, at: Pt },
    /// Delete node `node`.
    Delete { node: i64 },
}

fn dist_sq(a: Pt, b: Pt) -> f64 {
    (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)
}

fn is_closed(ids: &[i64]) -> bool {
    ids.len() > 1 && ids.first() == ids.last()
}

/// Index of the vertex nearest `cursor` (the first of equals).
fn nearest_vertex(points: &[Pt], cursor: Pt) -> Option<usize> {
    points
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| dist_sq(**a, cursor).total_cmp(&dist_sq(**b, cursor)))
        .map(|(i, _)| i)
}

/// The point of the way nearest `cursor` that lies strictly inside a segment,
/// with the index a vertex inserted there takes. `None` if the way has no
/// segment of positive length.
fn nearest_segment_point(points: &[Pt], cursor: Pt) -> Option<(usize, Pt)> {
    let mut best: Option<(f64, usize, Pt)> = None;
    for (i, pair) in points.windows(2).enumerate() {
        let (a, b) = (pair[0], pair[1]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len_sq = dx * dx + dy * dy;
        if len_sq <= f64::EPSILON {
            continue;
        }
        let t = (((cursor.0 - a.0) * dx + (cursor.1 - a.1) * dy) / len_sq).clamp(0.0, 1.0);
        // A projection onto an end would duplicate an existing vertex.
        let t = t.clamp(1e-6, 1.0 - 1e-6);
        let at = (a.0 + t * dx, a.1 + t * dy);
        let d = dist_sq(at, cursor);
        if best.is_none_or(|(bd, ..)| d < bd) {
            best = Some((d, i + 1, at));
        }
    }
    best.map(|(_, index, at)| (index, at))
}

/// Whether deleting `node` leaves a usable way: an open way keeps at least
/// two nodes, a closed one at least a triangle.
fn survives_delete(ids: &[i64], node: i64) -> bool {
    let left = ids.iter().filter(|&&n| n != node).count();
    if is_closed(ids) {
        // The ring's first and last entries are one node.
        let distinct_left = if ids.first() == Some(&node) {
            left
        } else {
            left - 1
        };
        distinct_left >= 3
    } else {
        left >= 2
    }
}

/// The edit a click at `cursor` makes to the way with node `ids` at screen
/// positions `points`, or `None` if there is nothing sensible to do (an empty
/// way, or a delete that would leave too few nodes).
pub fn plan(points: &[Pt], ids: &[i64], cursor: Pt, op: Op) -> Option<Plan> {
    if points.len() != ids.len() {
        return None;
    }
    match op {
        Op::Move => {
            let i = nearest_vertex(points, cursor)?;
            Some(Plan::Move {
                node: ids[i],
                to: cursor,
            })
        }
        Op::Delete => {
            let i = nearest_vertex(points, cursor)?;
            survives_delete(ids, ids[i]).then_some(Plan::Delete { node: ids[i] })
        }
        Op::Insert => {
            let (index, at) = nearest_segment_point(points, cursor)?;
            Some(Plan::Insert { index, at })
        }
    }
}

/// The way's outline after `plan` is applied.
pub fn preview(points: &[Pt], ids: &[i64], plan: Plan) -> Vec<Pt> {
    match plan {
        Plan::Move { node, to } => points
            .iter()
            .zip(ids)
            .map(|(&p, &id)| if id == node { to } else { p })
            .collect(),
        Plan::Insert { index, at } => {
            let mut out = points.to_vec();
            out.insert(index.min(out.len()), at);
            out
        }
        Plan::Delete { node } => {
            let mut out: Vec<Pt> = points
                .iter()
                .zip(ids)
                .filter(|(_, &id)| id != node)
                .map(|(&p, _)| p)
                .collect();
            // A ring that lost its start/end node closes on its new start.
            if is_closed(ids) && ids.first() == Some(&node) {
                if let Some(&first) = out.first() {
                    out.push(first);
                }
            }
            out
        }
    }
}

/// Where the edit happens, for drawing a marker: the new position of a moved
/// node, the new vertex, or the node about to be deleted.
pub fn marker(points: &[Pt], ids: &[i64], plan: Plan) -> Pt {
    match plan {
        Plan::Move { to, .. } => to,
        Plan::Insert { at, .. } => at,
        Plan::Delete { node } => ids
            .iter()
            .position(|&id| id == node)
            .map_or((0.0, 0.0), |i| points[i]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: [Pt; 3] = [(0.0, 0.0), (100.0, 0.0), (200.0, 0.0)];
    const IDS: [i64; 3] = [1, 2, 3];

    #[test]
    fn modifiers_pick_the_operation() {
        assert_eq!(Op::from_modifiers(false, false), Op::Move);
        assert_eq!(Op::from_modifiers(false, true), Op::Insert);
        assert_eq!(Op::from_modifiers(true, false), Op::Delete);
        assert_eq!(Op::from_modifiers(true, true), Op::Delete);
    }

    #[test]
    fn move_takes_the_nearest_vertex_to_the_cursor() {
        let p = plan(&LINE, &IDS, (90.0, 40.0), Op::Move).unwrap();
        assert_eq!(
            p,
            Plan::Move {
                node: 2,
                to: (90.0, 40.0)
            }
        );
        assert_eq!(
            preview(&LINE, &IDS, p),
            vec![(0.0, 0.0), (90.0, 40.0), (200.0, 0.0)]
        );
    }

    #[test]
    fn insert_puts_a_vertex_on_the_nearest_segment() {
        let p = plan(&LINE, &IDS, (150.0, 30.0), Op::Insert).unwrap();
        assert_eq!(
            p,
            Plan::Insert {
                index: 2,
                at: (150.0, 0.0)
            }
        );
        assert_eq!(preview(&LINE, &IDS, p).len(), 4);
        assert_eq!(preview(&LINE, &IDS, p)[2], (150.0, 0.0));
    }

    #[test]
    fn insert_never_duplicates_an_existing_vertex() {
        // Past the end of the way the projection clamps; it must stay inside.
        let Plan::Insert { at, .. } = plan(&LINE, &IDS, (500.0, 10.0), Op::Insert).unwrap() else {
            panic!("insert expected");
        };
        assert!(at.0 < 200.0 && at.0 > 199.0);
    }

    #[test]
    fn delete_removes_the_nearest_vertex() {
        let p = plan(&LINE, &IDS, (95.0, 5.0), Op::Delete).unwrap();
        assert_eq!(p, Plan::Delete { node: 2 });
        assert_eq!(preview(&LINE, &IDS, p), vec![(0.0, 0.0), (200.0, 0.0)]);
        assert_eq!(marker(&LINE, &IDS, p), (100.0, 0.0));
    }

    #[test]
    fn delete_is_refused_when_the_way_would_get_too_short() {
        let two = [(0.0, 0.0), (10.0, 0.0)];
        assert_eq!(plan(&two, &[1, 2], (1.0, 0.0), Op::Delete), None);
    }

    #[test]
    fn closed_ways_move_both_ends_of_the_ring_together() {
        let ring = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 0.0)];
        let ids = [1, 2, 3, 1];
        let p = plan(&ring, &ids, (-3.0, -3.0), Op::Move).unwrap();
        let out = preview(&ring, &ids, p);
        assert_eq!(out.first(), out.last());
        assert_eq!(out[0], (-3.0, -3.0));
    }

    #[test]
    fn deleting_a_rings_start_closes_it_on_the_new_start() {
        let ring = [
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
            (0.0, 0.0),
        ];
        let ids = [1, 2, 3, 4, 1];
        let p = plan(&ring, &ids, (-1.0, -1.0), Op::Delete).unwrap();
        assert_eq!(p, Plan::Delete { node: 1 });
        let out = preview(&ring, &ids, p);
        assert_eq!(out.len(), 4);
        assert_eq!(out.first(), out.last());
    }

    #[test]
    fn a_triangle_cannot_lose_a_vertex() {
        let tri = [(0.0, 0.0), (10.0, 0.0), (5.0, 8.0), (0.0, 0.0)];
        assert_eq!(plan(&tri, &[1, 2, 3, 1], (1.0, 1.0), Op::Delete), None);
    }
}
