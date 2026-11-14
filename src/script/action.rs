//! Resolving the name given to the `action` op against the registered actions.

/// What the text after `action` asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum ActionTarget {
    /// `LabelBench...`: the label benchmark knob, not a registered action.
    LabelBench,
    /// `MapStyle:<id>`: switch to a built-in map style, not a registered action.
    MapStyle(String),
    /// A registered action (fully qualified name) and the JSON that fills its fields.
    Registered {
        qualified: String,
        json: Option<String>,
    },
}

/// Resolve `requested` (`Name`, `ns::Name`, either followed by a JSON object
/// separated by whitespace, or one of the special prefixes) against
/// `registered`, the fully qualified names of every registered action.
/// A bare name is looked up in the `osm_gpui` namespace; when it is not there
/// the error lists the namespaces that do have an action of that name.
pub fn resolve_action(requested: &str, registered: &[&str]) -> Result<ActionTarget, String> {
    if requested.starts_with("LabelBench") {
        return Ok(ActionTarget::LabelBench);
    }
    if let Some(id) = requested.strip_prefix("MapStyle:") {
        return Ok(ActionTarget::MapStyle(id.to_string()));
    }
    let (name, json) = match requested.split_once(char::is_whitespace) {
        Some((n, json)) => (n, Some(json.to_string())),
        None => (requested, None),
    };
    let qualified = if name.contains("::") {
        name.to_string()
    } else {
        format!("osm_gpui::{name}")
    };
    if registered.contains(&qualified.as_str()) {
        return Ok(ActionTarget::Registered { qualified, json });
    }
    let bare = name.rsplit("::").next().unwrap_or(name);
    let mut elsewhere: Vec<&str> = registered
        .iter()
        .copied()
        .filter(|full| full.rsplit("::").next() == Some(bare) && *full != qualified)
        .collect();
    elsewhere.sort_unstable();
    if elsewhere.is_empty() {
        Err(format!("unknown action '{name}'"))
    } else {
        Err(format!(
            "unknown action '{qualified}'; an action named '{bare}' is registered as: {}",
            elsewhere.join(", ")
        ))
    }
}

/// The message for an action that is registered but that nothing handles
/// where the window's focus is now.
pub fn unhandled_message(qualified: &str) -> String {
    format!(
        "action '{qualified}' has no handler in the window's current focus context (nothing \
         would run); focus the element that handles it, or open the view it belongs to, first"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGISTERED: &[&str] = &[
        "osm_gpui::UploadToOsm",
        "layers::DeleteLayer",
        "selection::SelectAll",
        "mode::SetMode",
        "menu::Cancel",
        "osm_gpui::Cancel",
    ];

    fn registered(name: &str) -> ActionTarget {
        ActionTarget::Registered {
            qualified: name.to_string(),
            json: None,
        }
    }

    #[test]
    fn a_bare_name_is_looked_up_in_the_default_namespace() {
        assert_eq!(
            resolve_action("UploadToOsm", REGISTERED).unwrap(),
            registered("osm_gpui::UploadToOsm")
        );
        // Present in two namespaces: the default one wins.
        assert_eq!(
            resolve_action("Cancel", REGISTERED).unwrap(),
            registered("osm_gpui::Cancel")
        );
    }

    #[test]
    fn a_qualified_name_is_used_as_given() {
        assert_eq!(
            resolve_action("layers::DeleteLayer", REGISTERED).unwrap(),
            registered("layers::DeleteLayer")
        );
        assert_eq!(
            resolve_action("menu::Cancel", REGISTERED).unwrap(),
            registered("menu::Cancel")
        );
    }

    #[test]
    fn json_after_the_name_is_split_off() {
        assert_eq!(
            resolve_action("mode::SetMode {\"mode\": \"add\"}", REGISTERED).unwrap(),
            ActionTarget::Registered {
                qualified: "mode::SetMode".to_string(),
                json: Some("{\"mode\": \"add\"}".to_string()),
            }
        );
    }

    #[test]
    fn a_name_in_another_namespace_is_reported_with_its_namespace() {
        let e = resolve_action("DeleteLayer", REGISTERED).unwrap_err();
        assert!(e.contains("osm_gpui::DeleteLayer"), "{e}");
        assert!(e.contains("layers::DeleteLayer"), "{e}");
        let e = resolve_action("other::Cancel", REGISTERED).unwrap_err();
        assert!(e.contains("menu::Cancel, osm_gpui::Cancel"), "{e}");
    }

    #[test]
    fn an_unregistered_name_is_unknown() {
        assert_eq!(
            resolve_action("Nonsense", REGISTERED).unwrap_err(),
            "unknown action 'Nonsense'"
        );
    }

    #[test]
    fn the_special_prefixes_are_not_looked_up() {
        assert_eq!(
            resolve_action("LabelBench:on,halo=none", &[]).unwrap(),
            ActionTarget::LabelBench
        );
        assert_eq!(
            resolve_action("LabelBench", &[]).unwrap(),
            ActionTarget::LabelBench
        );
        assert_eq!(
            resolve_action("MapStyle:wireframe", &[]).unwrap(),
            ActionTarget::MapStyle("wireframe".to_string())
        );
    }

    #[test]
    fn the_unhandled_message_names_the_action() {
        assert!(unhandled_message("layers::DeleteLayer").contains("layers::DeleteLayer"));
    }
}
