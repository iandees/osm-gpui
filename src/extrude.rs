//! Extrude mode's geometry/topology, as pure functions over a planar frame.
//!
//! Dragging segment A-B of a way perpendicular to itself by an offset `d`
//! reshapes the way the way JOSM's Extrude tool does: each endpoint either
//! *moves* by `d` (when the neighbouring segment at that endpoint already
//! runs along the drag direction, and the node is free to move) or is
//! *duplicated* at `endpoint + d` and spliced in next to the original, so
//! A-B becomes A-A'-B'-B. Pulling a whole wall of a rectangle therefore keeps
//! it a 4-node rectangle, while pulling a sub-segment of a longer wall makes
//! an L- or T-shaped bump (or a notch, when dragged inward).
//!
//! Positions are in any isotropic planar frame (the app uses screen pixels,
//! where Mercator is locally conformal); nothing here knows about lat/lon.

/// Neighbouring segments within this many degrees of the drag direction count
/// as parallel to it.
pub const PARALLEL_TOLERANCE_DEG: f64 = 5.0;

/// Drags whose component along the segment normal is below this (planar
/// units) are treated as no drag at all.
const MIN_OFFSET: f64 = 1e-9;

/// One entry of the way's node list, as the planner needs to see it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentNode {
    pub id: i64,
    pub pos: (f64, f64),
    /// Whether the node may be moved rather than duplicated: it belongs to
    /// no other way, no relation, and carries no tags.
    pub movable: bool,
}

/// What an extrude does to the dragged segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtrudeKind {
    /// Default: pull the segment out, creating nodes where needed.
    Reshape,
    /// Translate A and B along the normal; no nodes are created.
    Move,
}

/// An entry of the planned node list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanNode {
    Existing(i64),
    /// Index into [`ExtrudePlan::new_nodes`].
    New(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtrudePlan {
    /// The way's new node list (same closing convention as the input).
    pub nodes: Vec<PlanNode>,
    /// Positions of the nodes to create, referenced by [`PlanNode::New`].
    pub new_nodes: Vec<(f64, f64)>,
    /// Existing nodes to move, with their new positions.
    pub moved: Vec<(i64, (f64, f64))>,
}

/// The part of `drag` along the normal of segment `a`-`b`, as a vector.
/// `None` for a zero-length segment.
pub fn normal_offset(a: (f64, f64), b: (f64, f64), drag: (f64, f64)) -> Option<(f64, f64)> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx.hypot(dy);
    if len < MIN_OFFSET {
        return None;
    }
    let n = (-dy / len, dx / len);
    let t = drag.0 * n.0 + drag.1 * n.1;
    Some((n.0 * t, n.1 * t))
}

/// Plan an extrude of segment `segment` (node `segment` to `segment + 1`) of
/// the way `way` by `drag`, constrained to the segment's normal. A way whose
/// first and last ids match (and has at least 4 entries) is a closed ring.
/// Returns `None` for an invalid segment or a (near-)zero offset.
pub fn plan_extrude(
    way: &[SegmentNode],
    segment: usize,
    drag: (f64, f64),
    kind: ExtrudeKind,
) -> Option<ExtrudePlan> {
    let n = way.len();
    if n < 2 || segment + 1 >= n {
        return None;
    }
    let closed = n >= 4 && way[0].id == way[n - 1].id;
    let m = if closed { n - 1 } else { n };
    let ia = segment;
    let ib = if closed { (segment + 1) % m } else { segment + 1 };
    let (a, b) = (way[ia], way[ib]);
    if a.id == b.id {
        return None;
    }
    let off = normal_offset(a.pos, b.pos, drag)?;
    if off.0.hypot(off.1) < MIN_OFFSET {
        return None;
    }
    let shifted = |p: (f64, f64)| (p.0 + off.0, p.1 + off.1);

    let existing: Vec<PlanNode> = way.iter().map(|w| PlanNode::Existing(w.id)).collect();

    if kind == ExtrudeKind::Move {
        return Some(ExtrudePlan {
            nodes: existing,
            new_nodes: Vec::new(),
            moved: vec![(a.id, shifted(a.pos)), (b.id, shifted(b.pos))],
        });
    }

    let prev = if closed {
        Some(way[(ia + m - 1) % m])
    } else if ia > 0 {
        Some(way[ia - 1])
    } else {
        None
    };
    let next = if closed {
        Some(way[(ib + 1) % m])
    } else if ib + 1 < n {
        Some(way[ib + 1])
    } else {
        None
    };

    // The neighbour runs along the drag direction when it is (nearly)
    // perpendicular to A-B.
    let ab_len = (b.pos.0 - a.pos.0).hypot(b.pos.1 - a.pos.1);
    let ab = ((b.pos.0 - a.pos.0) / ab_len, (b.pos.1 - a.pos.1) / ab_len);
    let sin_tol = PARALLEL_TOLERANCE_DEG.to_radians().sin();
    let along_drag = |end: SegmentNode, other: SegmentNode| -> bool {
        let (vx, vy) = (end.pos.0 - other.pos.0, end.pos.1 - other.pos.1);
        let len = vx.hypot(vy);
        len > MIN_OFFSET && ((vx * ab.0 + vy * ab.1) / len).abs() <= sin_tol
    };
    let move_a = a.movable && prev.is_some_and(|p| p.id != b.id && along_drag(a, p));
    let move_b = b.movable && next.is_some_and(|q| q.id != a.id && along_drag(b, q));

    let mut moved = Vec::new();
    let mut new_nodes = Vec::new();
    let mut inserted = Vec::new();
    if move_a {
        moved.push((a.id, shifted(a.pos)));
    } else {
        inserted.push(PlanNode::New(new_nodes.len()));
        new_nodes.push(shifted(a.pos));
    }
    if move_b {
        moved.push((b.id, shifted(b.pos)));
    } else {
        inserted.push(PlanNode::New(new_nodes.len()));
        new_nodes.push(shifted(b.pos));
    }

    let mut nodes = Vec::with_capacity(n + 2);
    for (k, node) in existing.into_iter().enumerate() {
        nodes.push(node);
        if k == ia {
            nodes.extend(inserted.iter().copied());
        }
    }
    Some(ExtrudePlan {
        nodes,
        new_nodes,
        moved,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: i64, x: f64, y: f64) -> SegmentNode {
        SegmentNode {
            id,
            pos: (x, y),
            movable: true,
        }
    }

    /// Closed 10x10 square: 1(0,0) 2(10,0) 3(10,10) 4(0,10) 1.
    fn square() -> Vec<SegmentNode> {
        vec![
            node(1, 0.0, 0.0),
            node(2, 10.0, 0.0),
            node(3, 10.0, 10.0),
            node(4, 0.0, 10.0),
            node(1, 0.0, 0.0),
        ]
    }

    fn ids(plan: &ExtrudePlan, news: &[i64]) -> Vec<i64> {
        plan.nodes
            .iter()
            .map(|n| match n {
                PlanNode::Existing(id) => *id,
                PlanNode::New(i) => news[*i],
            })
            .collect()
    }

    fn moved_pos(plan: &ExtrudePlan, id: i64) -> (f64, f64) {
        plan.moved.iter().find(|m| m.0 == id).unwrap().1
    }

    #[test]
    fn full_wall_of_rectangle_moves_both_ends() {
        // Segment 1 is 2->3 (the east wall); drag east by 5.
        let plan = plan_extrude(&square(), 1, (5.0, 0.0), ExtrudeKind::Reshape).unwrap();
        assert!(plan.new_nodes.is_empty());
        assert_eq!(ids(&plan, &[]), vec![1, 2, 3, 4, 1]);
        assert_eq!(moved_pos(&plan, 2), (15.0, 0.0));
        assert_eq!(moved_pos(&plan, 3), (15.0, 10.0));
    }

    #[test]
    fn drag_is_constrained_to_the_normal() {
        let plan = plan_extrude(&square(), 1, (5.0, 7.0), ExtrudeKind::Reshape).unwrap();
        assert_eq!(moved_pos(&plan, 2), (15.0, 0.0));
        assert_eq!(moved_pos(&plan, 3), (15.0, 10.0));
    }

    #[test]
    fn full_wall_inward_shrinks_the_rectangle() {
        let plan = plan_extrude(&square(), 1, (-4.0, 0.0), ExtrudeKind::Reshape).unwrap();
        assert!(plan.new_nodes.is_empty());
        assert_eq!(moved_pos(&plan, 2), (6.0, 0.0));
    }

    #[test]
    fn middle_sub_segment_makes_an_l_or_t_shape() {
        // East wall split by collinear nodes 5 and 6; pull 5-6 east. Its
        // neighbours (2-5 and 6-3) are collinear with it, not along the
        // drag, so two nodes are inserted: a bump.
        let ring = vec![
            node(1, 0.0, 0.0),
            node(2, 10.0, 0.0),
            node(5, 10.0, 4.0),
            node(6, 10.0, 6.0),
            node(3, 10.0, 10.0),
            node(4, 0.0, 10.0),
            node(1, 0.0, 0.0),
        ];
        let plan = plan_extrude(&ring, 2, (3.0, 0.0), ExtrudeKind::Reshape).unwrap();
        assert_eq!(plan.new_nodes, vec![(13.0, 4.0), (13.0, 6.0)]);
        assert!(plan.moved.is_empty());
        assert_eq!(ids(&plan, &[100, 101]), vec![1, 2, 5, 100, 101, 6, 3, 4, 1]);
    }

    #[test]
    fn inward_notch_inserts_two_nodes() {
        let ring = vec![
            node(1, 0.0, 0.0),
            node(2, 10.0, 0.0),
            node(5, 10.0, 4.0),
            node(6, 10.0, 6.0),
            node(3, 10.0, 10.0),
            node(4, 0.0, 10.0),
            node(1, 0.0, 0.0),
        ];
        let plan = plan_extrude(&ring, 2, (-3.0, 0.0), ExtrudeKind::Reshape).unwrap();
        assert_eq!(plan.new_nodes, vec![(7.0, 4.0), (7.0, 6.0)]);
    }

    #[test]
    fn one_end_parallel_moves_it_and_inserts_the_other() {
        // L-shaped corner: segment 5->6 where 5's previous segment 2->5 is
        // perpendicular to 5-6 only at one end. Open way: 0(0,0) 1(0,5) 2(5,5)
        // 3(9,5): drag segment 1->2 (horizontal) up. 1's neighbour 0-1 is
        // vertical (along drag): move. 2's neighbour 2-3 is collinear: insert.
        let way = vec![
            node(10, 0.0, 0.0),
            node(11, 0.0, 5.0),
            node(12, 5.0, 5.0),
            node(13, 9.0, 5.0),
        ];
        let plan = plan_extrude(&way, 1, (0.0, 2.0), ExtrudeKind::Reshape).unwrap();
        assert_eq!(moved_pos(&plan, 11), (0.0, 7.0));
        assert_eq!(plan.new_nodes, vec![(5.0, 7.0)]);
        assert_eq!(ids(&plan, &[99]), vec![10, 11, 99, 12, 13]);
    }

    #[test]
    fn shared_endpoint_gets_a_new_node_instead_of_moving() {
        let mut ring = square();
        ring[1].movable = false; // node 2 is shared with another way
        let plan = plan_extrude(&ring, 1, (5.0, 0.0), ExtrudeKind::Reshape).unwrap();
        assert_eq!(moved_pos(&plan, 3), (15.0, 10.0));
        assert_eq!(plan.moved.len(), 1);
        assert_eq!(plan.new_nodes, vec![(15.0, 0.0)]);
        assert_eq!(ids(&plan, &[99]), vec![1, 2, 99, 3, 4, 1]);
    }

    #[test]
    fn tagged_endpoint_gets_a_new_node_instead_of_moving() {
        let mut ring = square();
        ring[2].movable = false; // node 3 carries tags
        let plan = plan_extrude(&ring, 1, (5.0, 0.0), ExtrudeKind::Reshape).unwrap();
        assert_eq!(moved_pos(&plan, 2), (15.0, 0.0));
        assert_eq!(plan.new_nodes, vec![(15.0, 10.0)]);
        assert_eq!(ids(&plan, &[99]), vec![1, 2, 99, 3, 4, 1]);
    }

    #[test]
    fn wrap_around_segment_on_closed_way() {
        // Segment 3 is 4(0,10) -> 1(0,0), the west wall, ending at the ring
        // start. Drag west by 5: both ends move, ring stays closed.
        let plan = plan_extrude(&square(), 3, (-5.0, 0.0), ExtrudeKind::Reshape).unwrap();
        assert!(plan.new_nodes.is_empty());
        assert_eq!(ids(&plan, &[]), vec![1, 2, 3, 4, 1]);
        assert_eq!(moved_pos(&plan, 4), (-5.0, 10.0));
        assert_eq!(moved_pos(&plan, 1), (-5.0, 0.0));
    }

    #[test]
    fn wrap_around_with_shared_start_node_inserts_before_closing_node() {
        let mut ring = square();
        ring[0].movable = false;
        ring[4].movable = false;
        let plan = plan_extrude(&ring, 3, (-5.0, 0.0), ExtrudeKind::Reshape).unwrap();
        assert_eq!(moved_pos(&plan, 4), (-5.0, 10.0));
        assert_eq!(plan.new_nodes, vec![(-5.0, 0.0)]);
        assert_eq!(ids(&plan, &[99]), vec![1, 2, 3, 4, 99, 1]);
    }

    #[test]
    fn first_segment_of_closed_way_uses_wrapped_previous_neighbour() {
        // Segment 0 is 1->2, the south wall; node 1's previous neighbour is
        // node 4 (wrap-around), which is along the drag.
        let plan = plan_extrude(&square(), 0, (0.0, -3.0), ExtrudeKind::Reshape).unwrap();
        assert!(plan.new_nodes.is_empty());
        assert_eq!(moved_pos(&plan, 1), (0.0, -3.0));
        assert_eq!(moved_pos(&plan, 2), (10.0, -3.0));
    }

    #[test]
    fn open_way_end_segment_inserts_at_the_free_end() {
        // Open way 1(0,0) 2(10,0) 3(10,10): drag the first segment up.
        let way = vec![node(1, 0.0, 0.0), node(2, 10.0, 0.0), node(3, 10.0, 10.0)];
        let plan = plan_extrude(&way, 0, (0.0, 4.0), ExtrudeKind::Reshape).unwrap();
        // 1 has no previous neighbour: new node. 2's next neighbour (2-3) is
        // vertical, along the drag: moves.
        assert_eq!(plan.new_nodes, vec![(0.0, 4.0)]);
        assert_eq!(moved_pos(&plan, 2), (10.0, 4.0));
        assert_eq!(ids(&plan, &[99]), vec![1, 99, 2, 3]);
    }

    #[test]
    fn open_two_node_way_inserts_both() {
        let way = vec![node(1, 0.0, 0.0), node(2, 10.0, 0.0)];
        let plan = plan_extrude(&way, 0, (0.0, 4.0), ExtrudeKind::Reshape).unwrap();
        assert_eq!(plan.new_nodes, vec![(0.0, 4.0), (10.0, 4.0)]);
        assert_eq!(ids(&plan, &[98, 99]), vec![1, 98, 99, 2]);
    }

    #[test]
    fn zero_and_tiny_drags_are_noops() {
        assert!(plan_extrude(&square(), 1, (0.0, 0.0), ExtrudeKind::Reshape).is_none());
        assert!(plan_extrude(&square(), 1, (1e-12, 5.0), ExtrudeKind::Reshape).is_none());
    }

    #[test]
    fn invalid_segment_is_none() {
        assert!(plan_extrude(&square(), 4, (5.0, 0.0), ExtrudeKind::Reshape).is_none());
        assert!(plan_extrude(&[], 0, (5.0, 0.0), ExtrudeKind::Reshape).is_none());
    }

    #[test]
    fn move_kind_translates_both_ends_without_new_nodes() {
        let mut ring = square();
        ring[1].movable = false; // ignored when moving
        let plan = plan_extrude(&ring, 1, (5.0, 0.0), ExtrudeKind::Move).unwrap();
        assert!(plan.new_nodes.is_empty());
        assert_eq!(ids(&plan, &[]), vec![1, 2, 3, 4, 1]);
        assert_eq!(moved_pos(&plan, 2), (15.0, 0.0));
        assert_eq!(moved_pos(&plan, 3), (15.0, 10.0));
    }

    #[test]
    fn nearly_parallel_neighbour_within_tolerance_still_moves() {
        // Node 3 nudged 0.2 off vertical (~1.1 degrees).
        let mut ring = square();
        ring[1].pos = (10.2, 0.0);
        let plan = plan_extrude(&ring, 1, (5.0, 0.0), ExtrudeKind::Reshape).unwrap();
        assert!(plan.new_nodes.is_empty());
    }
}
