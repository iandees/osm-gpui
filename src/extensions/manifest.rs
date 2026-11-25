//! `extension.toml`: who an extension is, what it may reach, and the menu
//! commands it offers. Read before the extension's code is loaded, so the
//! menus can be built without running it.

use serde::{Deserialize, Serialize};

/// The manifest format this app reads.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub schema_version: u32,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub commands: Vec<CommandDef>,
    /// Events the extension wants to hear about: `viewport` (the map
    /// settled after moving) and `selection` (the selection changed).
    #[serde(default)]
    pub subscribe: Vec<String>,
}

/// What the extension may reach beyond its sandbox.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct Capabilities {
    /// Hosts `http-fetch` may reach over HTTPS. `example.org` also allows its
    /// subdomains (`api.example.org`).
    #[serde(default)]
    pub http: Vec<String>,
    /// May keep secrets in the system keychain (`secret-get`/`secret-set`).
    #[serde(default)]
    pub secrets: bool,
}

/// One menu command an extension adds.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CommandDef {
    pub id: String,
    pub label: String,
    /// Where it goes: one of [`MENU_LOCATIONS`]. Tools when not given.
    #[serde(default)]
    pub menu: Option<String>,
    /// Drawn with a checkmark, which the extension sets with
    /// `set-command-checked`.
    #[serde(default)]
    pub toggle: bool,
}

/// The places in the menus an extension's commands can go, each next to the
/// app's own items of the same kind:
///
/// - `overlays`: View, with Show Notes and To-do List. Things drawn over the
///   map (photos, tasks).
/// - `imagery`: Imagery, with the ways to add imagery layers.
/// - `data`: File, with Download from OSM and Download GPS Traces. Ways to
///   load data layers.
/// - `tools`: Tools, with the selection actions. The default.
/// - `help`: Help.
pub const MENU_LOCATIONS: &[&str] = &["overlays", "imagery", "data", "tools", "help"];

impl CommandDef {
    /// Where the command goes.
    pub fn location(&self) -> &str {
        self.menu.as_deref().unwrap_or("tools")
    }
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let m: Manifest = toml::from_str(text).map_err(|e| e.to_string())?;
        if m.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "schema_version {} is not supported (this app reads {SCHEMA_VERSION})",
                m.schema_version
            ));
        }
        if !valid_id(&m.id) {
            return Err(format!(
                "id {:?} must be lowercase letters, digits and dashes",
                m.id
            ));
        }
        if m.name.trim().is_empty() {
            return Err("name is empty".into());
        }
        for c in &m.commands {
            if !valid_id(&c.id) {
                return Err(format!("command id {:?} is not valid", c.id));
            }
            if !MENU_LOCATIONS.contains(&c.location()) {
                return Err(format!(
                    "command {:?}: unknown menu {:?} (known: {})",
                    c.id,
                    c.location(),
                    MENU_LOCATIONS.join(", ")
                ));
            }
        }
        for event in &m.subscribe {
            if !matches!(event.as_str(), "viewport" | "selection") {
                return Err(format!(
                    "subscribe: unknown event {event:?} (known: viewport, selection)"
                ));
            }
        }
        for h in &m.capabilities.http {
            if h.is_empty() || h.contains(['/', ':', '@', '*']) {
                return Err(format!("http capability {h:?} must be a bare host name"));
            }
        }
        Ok(m)
    }

    /// Whether the extension asked to hear about `event`.
    pub fn subscribes(&self, event: &str) -> bool {
        self.subscribe.iter().any(|e| e == event)
    }

    /// Whether `url` is HTTPS to a host the manifest allows.
    pub fn allows_url(&self, url: &str) -> bool {
        let Some(host) = https_host(url) else {
            return false;
        };
        self.capabilities.http.iter().any(|allowed| {
            let allowed = allowed.to_ascii_lowercase();
            host == allowed || host.ends_with(&format!(".{allowed}"))
        })
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
}

/// The lowercase host of an `https://` URL; `None` for any other scheme or a
/// URL with credentials in it.
fn https_host(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("HTTPS://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.contains('@') || authority.is_empty() {
        return None;
    }
    let host = match authority.rsplit_once(':') {
        Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) => h,
        _ => authority,
    };
    Some(host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOML: &str = r#"
id = "maproulette"
name = "MapRoulette"
version = "0.1.0"
schema_version = 1

[capabilities]
http = ["maproulette.org"]

[[commands]]
id = "show-tasks"
label = "Show Tasks Here"
"#;

    #[test]
    fn a_manifest_parses() {
        let m = Manifest::parse(TOML).unwrap();
        assert_eq!(m.id, "maproulette");
        assert_eq!(m.commands[0].label, "Show Tasks Here");
    }

    #[test]
    fn the_repository_manifest_parses() {
        let text = include_str!("../../extensions/maproulette/extension.toml");
        Manifest::parse(text).unwrap();
    }

    #[test]
    fn http_is_limited_to_the_listed_hosts_over_https() {
        let m = Manifest::parse(TOML).unwrap();
        assert!(m.allows_url("https://maproulette.org/api/v2/task/1"));
        assert!(m.allows_url("https://api.maproulette.org:443/x"));
        assert!(m.allows_url("https://MapRoulette.org?x=1"));
        assert!(!m.allows_url("http://maproulette.org/api"));
        assert!(!m.allows_url("https://evilmaproulette.org/"));
        assert!(!m.allows_url("https://maproulette.org.evil.com/"));
        assert!(!m.allows_url("https://user@maproulette.org/"));
        assert!(!m.allows_url("https://example.com/?u=https://maproulette.org"));
    }

    #[test]
    fn subscriptions_are_read_and_unknown_events_refused() {
        let top = TOML.replacen(
            "schema_version = 1\n",
            "schema_version = 1\nsubscribe = [\"viewport\", \"selection\"]\n",
            1,
        );
        let m = Manifest::parse(&top).unwrap();
        assert!(m.subscribes("viewport") && m.subscribes("selection"));
        assert!(!Manifest::parse(TOML).unwrap().subscribes("viewport"));
        let bad = TOML.replacen(
            "schema_version = 1\n",
            "schema_version = 1\nsubscribe = [\"keys\"]\n",
            1,
        );
        assert!(Manifest::parse(&bad).is_err());
    }

    #[test]
    fn commands_go_to_known_menu_locations() {
        let m = Manifest::parse(TOML).unwrap();
        assert_eq!(m.commands[0].location(), "tools", "the default");
        assert!(!m.commands[0].toggle);
        let overlay = TOML.replacen(
            "label = \"Show Tasks Here\"",
            "label = \"Show Tasks Here\"\nmenu = \"overlays\"\ntoggle = true",
            1,
        );
        let m = Manifest::parse(&overlay).unwrap();
        assert_eq!(m.commands[0].location(), "overlays");
        assert!(m.commands[0].toggle);
        let bad = TOML.replacen(
            "label = \"Show Tasks Here\"",
            "label = \"Show Tasks Here\"\nmenu = \"window\"",
            1,
        );
        assert!(Manifest::parse(&bad).unwrap_err().contains("unknown menu"));
    }

    #[test]
    fn bad_manifests_are_refused() {
        assert!(
            Manifest::parse(&TOML.replace("schema_version = 1", "schema_version = 9")).is_err()
        );
        assert!(Manifest::parse(&TOML.replace("\"maproulette\"\n", "\"Map Roulette\"\n")).is_err());
        assert!(Manifest::parse(&TOML.replace("[\"maproulette.org\"]", "[\"*.org\"]")).is_err());
    }
}
