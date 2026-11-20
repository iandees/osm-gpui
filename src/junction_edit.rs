//! Turn-restriction edits from the Junction section: clicking a movement
//! steps it along allowed → `no_*` → `only_*` → allowed (see
//! `osm_gpui::junction::toggle`), as one undo step.

use osm_gpui::edit_ops::{EditOp, Recording};
use osm_gpui::ids::{NodeId, WayId};
use osm_gpui::junction::{self, ArmEnd, RestrictionEdit};
use osm_gpui::layers::{EditableLayer, LayerId};
use osm_gpui::osm::{MemberType, OsmMember};
use osm_gpui::selection::FeatureKind;
use osm_gpui::tags::Tags;
use osm_gpui::topology;
use osm_gpui::tr;

use crate::selection_actions::{record_edit, record_split, refused, split_relations, ways_using};
use crate::undo::{EditLabel, UndoableAction};
use crate::MapViewer;

impl MapViewer {
    /// Step the selected junction's movement `from` → `to` to its next
    /// restriction state, or say in the status bar why it can't be.
    pub(crate) fn toggle_junction_movement(&mut self, from: usize, to: usize) {
        if self.refuse_edit_during_upload() {
            return;
        }
        let Some((layer_id, j)) = self.selected_junction() else {
            return;
        };
        let Some(edit) = junction::toggle(&j, from, to) else {
            return;
        };
        let Some(editable) = self
            .layer_manager
            .find_layer_mut(layer_id)
            .and_then(|layer| layer.as_editable_mut())
        else {
            return;
        };
        match apply_restriction_edit(editable, layer_id, &edit) {
            Ok((label, ops)) => {
                self.undo_stack.push(UndoableAction::edit(
                    layer_id,
                    EditLabel::Text(label.clone()),
                    ops,
                ));
                self.set_status(label);
            }
            Err(message) => self.set_status(message),
        }
    }
}

/// Carry out `edit` on the layer through a recording: the History label and
/// the undo step's ops, or why it was refused (nothing changed then).
fn apply_restriction_edit(
    editable: &mut dyn EditableLayer,
    layer_id: LayerId,
    edit: &RestrictionEdit,
) -> Result<(String, Vec<EditOp>), String> {
    match edit {
        RestrictionEdit::Create {
            from,
            via,
            to,
            value,
        } => create_restriction(editable, layer_id, from, *via, to, value),
        RestrictionEdit::Retag {
            relation,
            key,
            value,
        } => {
            let ((), ops) = record_edit(layer_id, editable, |rec| {
                rec.set_tag(FeatureKind::Relation, relation.get(), key, value)
                    .map_err(refused)
            })?;
            Ok((
                tr!("action.junction.changed", value = value).to_string(),
                ops,
            ))
        }
        RestrictionEdit::Delete { relation } => {
            let (_, ops) = record_edit(layer_id, editable, |rec| {
                rec.delete_relation(*relation).map_err(refused)
            })?;
            Ok((
                tr!("action.junction.removed", id = relation.get()).to_string(),
                ops,
            ))
        }
    }
}

/// A new `type=restriction` relation from `from` via `via` to `to`. A from
/// or to way that runs on past the junction is split there first (the
/// existing Split Way, so other relations follow the parts), so the
/// restriction names only the side the movement uses.
fn create_restriction(
    editable: &mut dyn EditableLayer,
    layer_id: LayerId,
    from: &ArmEnd,
    via: NodeId,
    to: &ArmEnd,
    value: &str,
) -> Result<(String, Vec<EditOp>), String> {
    let mut split: Vec<WayId> = [from, to]
        .iter()
        .filter(|e| e.needs_split)
        .map(|e| e.way)
        .collect();
    split.dedup();
    let plans = if split.is_empty() {
        Vec::new()
    } else {
        let ways: Vec<(i64, Vec<i64>)> = ways_using(editable, via)
            .into_iter()
            .filter(|(id, _)| split.contains(&WayId(*id)))
            .collect();
        topology::plan_split(&[via.get()], None, &ways)
    };
    let (relations, ids_by_plan) = split_relations(editable, &plans)?;
    let did_split = !plans.is_empty();

    let (_, ops) = record_edit(layer_id, editable, |rec| {
        let parts = record_split(rec, layer_id, &plans, &ids_by_plan, relations)?;
        let from_way = arm_way(rec, &plans, &parts, from, via);
        let to_way = arm_way(rec, &plans, &parts, to, via);
        let member = |member_type, reference, role: &str| OsmMember {
            member_type,
            reference,
            role: role.to_string(),
        };
        let members = vec![
            member(MemberType::Way, from_way.get(), "from"),
            member(MemberType::Node, via.get(), "via"),
            member(MemberType::Way, to_way.get(), "to"),
        ];
        let tags = Tags::from([("type", "restriction"), ("restriction", value)]);
        rec.create_relation(tags, members).map_err(refused)
    })?;
    let label = if did_split {
        tr!("action.junction.created_split", value = value)
    } else {
        tr!("action.junction.created", value = value)
    };
    Ok((label.to_string(), ops))
}

/// The way that now holds `end`'s side of the junction: after a split, the
/// part of its way that runs from `via` to `end.next_node`.
fn arm_way(
    rec: &Recording<'_>,
    plans: &[topology::SplitPlan],
    parts: &[Vec<WayId>],
    end: &ArmEnd,
    via: NodeId,
) -> WayId {
    let Some(candidates) = plans
        .iter()
        .zip(parts)
        .find(|(plan, _)| plan.way_id == end.way.get())
        .map(|(_, ids)| ids)
    else {
        return end.way;
    };
    candidates
        .iter()
        .copied()
        .find(|&w| {
            rec.layer().way_node_ids(w).is_some_and(|nodes| {
                nodes
                    .windows(2)
                    .any(|p| p.contains(&via) && p.contains(&end.next_node))
            })
        })
        .unwrap_or(end.way)
}
