//! MapRoulette for osm-gpui: the tasks in view as pins, a task's challenge
//! and instructions in the side panel, its data downloaded on request, and
//! its status set (with the user's MapRoulette API key) once it is done.

use osm_gpui_extension_api::{
    self as osm, button, Bounds, ButtonStyle, Element, Extension, HttpMethod, HttpRequest,
    Inputs, LatLon, Marker, Panel, TextInput,
};
use serde::Deserialize;

const API: &str = "https://maproulette.org/api/v2";
const SITE: &str = "https://maproulette.org";
/// Most tasks asked for in one go.
const TASK_LIMIT: usize = 500;
/// Below this zoom the view is too big to ask for its tasks.
const MIN_ZOOM: f64 = 10.0;
const API_KEY: &str = "api-key";

/// A task as the box search lists it.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TaskSummary {
    pub id: i64,
    #[serde(rename = "parentId")]
    pub challenge_id: i64,
    #[serde(rename = "parentName", default)]
    pub challenge_name: String,
    pub point: Point,
    #[serde(default)]
    pub status: i32,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
pub struct Point {
    pub lat: f64,
    pub lng: f64,
}

/// A task in full.
#[derive(Debug, Clone, Deserialize)]
pub struct Task {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    pub parent: i64,
    #[serde(default)]
    pub instruction: String,
    #[serde(default)]
    pub status: i32,
    #[serde(default)]
    pub geometries: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Challenge {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub instruction: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "checkinComment", default)]
    pub checkin_comment: String,
}

/// MapRoulette's task statuses that a mapper sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Created = 0,
    Fixed = 1,
    NotAnIssue = 2,
    Skipped = 3,
    AlreadyFixed = 5,
    TooHard = 6,
}

impl Status {
    fn from_code(code: i32) -> Option<Status> {
        Some(match code {
            0 => Status::Created,
            1 => Status::Fixed,
            2 => Status::NotAnIssue,
            3 => Status::Skipped,
            5 => Status::AlreadyFixed,
            6 => Status::TooHard,
            _ => return None,
        })
    }
}

/// Pin colour for a task status: open tasks stand out, finished ones recede.
pub fn status_color(code: i32) -> u32 {
    match code {
        0 => 0x7c3aed,     // open: violet, MapRoulette's own colour
        3 | 6 => 0xf59e0b, // skipped / too hard: amber
        1 | 2 | 5 => 0x16a34a,
        _ => 0x6b7280,
    }
}

/// A status's name in the app's language (`status.<code>` in the locales).
fn status_label(code: i32) -> String {
    match Status::from_code(code) {
        Some(_) => osm::t(&format!("status.{code}")),
        None => osm::t_args("status.other", &[("code", &code.to_string())]),
    }
}

pub fn box_url(b: &Bounds) -> String {
    format!(
        "{API}/tasks/box/{}/{}/{}/{}?limit={TASK_LIMIT}",
        b.min_lon, b.min_lat, b.max_lon, b.max_lat
    )
}

/// Every coordinate in a GeoJSON value, as (lon, lat).
fn coords(v: &serde_json::Value, out: &mut Vec<(f64, f64)>) {
    match v {
        serde_json::Value::Array(a) => {
            if let [x, y, ..] = a.as_slice() {
                if let (Some(x), Some(y)) = (x.as_f64(), y.as_f64()) {
                    out.push((x, y));
                    return;
                }
            }
            for item in a {
                coords(item, out);
            }
        }
        serde_json::Value::Object(o) => {
            if let Some(c) = o.get("coordinates") {
                coords(c, out);
            }
            for key in ["features", "geometry", "geometries"] {
                if let Some(c) = o.get(key) {
                    coords(c, out);
                }
            }
        }
        _ => {}
    }
}

/// The area to download for a task: its geometry's extent with a margin, at
/// least ~200 m across so the surroundings come too.
pub fn task_area(geometries: &serde_json::Value, fallback: Point) -> Bounds {
    let mut pts = Vec::new();
    coords(geometries, &mut pts);
    if pts.is_empty() {
        pts.push((fallback.lng, fallback.lat));
    }
    let (mut min_lon, mut min_lat, mut max_lon, mut max_lat) = (180.0, 90.0, -180.0, -90.0);
    for (lon, lat) in pts {
        min_lon = f64::min(min_lon, lon);
        max_lon = f64::max(max_lon, lon);
        min_lat = f64::min(min_lat, lat);
        max_lat = f64::max(max_lat, lat);
    }
    let pad_lat = ((max_lat - min_lat) * 0.2).max(0.001);
    let pad_lon = ((max_lon - min_lon) * 0.2).max(0.0015);
    Bounds {
        min_lat: min_lat - pad_lat,
        max_lat: max_lat + pad_lat,
        min_lon: min_lon - pad_lon,
        max_lon: max_lon + pad_lon,
    }
}

/// The OSM ids a task's features name (`node/123`, `w456`...), for the panel.
pub fn feature_ids(geometries: &serde_json::Value) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(features) = geometries.get("features").and_then(|f| f.as_array()) {
        for f in features {
            let id = f
                .get("properties")
                .and_then(|p| p.get("@id").or_else(|| p.get("osmid")))
                .or_else(|| f.get("id"))
                .and_then(|v| v.as_str());
            if let Some(id) = id {
                if !ids.iter().any(|i| i == id) {
                    ids.push(id.to_string());
                }
            }
        }
    }
    ids
}

struct Open {
    task: Task,
    challenge: Challenge,
    at: Point,
    /// The last status write's outcome, shown in the panel.
    note: Option<String>,
}

/// The side-panel sections this extension shows.
const TASK_PANEL: &str = "task";
const KEY_PANEL: &str = "api-key";

#[derive(Default)]
struct MapRoulette {
    tasks: Vec<TaskSummary>,
    open: Option<Open>,
}

/// The saved MapRoulette API key, from the keychain. A key an earlier
/// version kept in plain storage is moved there first.
fn api_key() -> Option<String> {
    if let Some(old) = osm::storage_get(API_KEY) {
        osm::secret_set(API_KEY, Some(&old));
        osm::storage_set(API_KEY, None);
    }
    osm::secret_get(API_KEY)
}

impl MapRoulette {
    fn markers(&self) -> Vec<Marker> {
        let open = self.open.as_ref().map(|o| o.task.id);
        self.tasks
            .iter()
            .map(|t| Marker {
                id: t.id.to_string(),
                at: LatLon {
                    lat: t.point.lat,
                    lon: t.point.lng,
                },
                color: status_color(t.status),
                selected: open == Some(t.id),
            })
            .collect()
    }

    fn redraw(&self) {
        osm::set_markers(&self.markers());
        osm::show_panel(TASK_PANEL, self.task_panel().as_ref());
    }

    fn task_panel(&self) -> Option<Panel> {
        let open = self.open.as_ref()?;
        let (task, challenge) = (&open.task, &open.challenge);
        let mut elements = vec![
            Element::Heading(challenge.name.clone()),
            Element::Property((osm::t("panel.task"), format!("{} · {}", task.id, task.name))),
            Element::Property((osm::t("panel.status"), status_label(task.status))),
        ];
        let instruction = if task.instruction.trim().is_empty() {
            &challenge.instruction
        } else {
            &task.instruction
        };
        if !instruction.trim().is_empty() {
            elements.push(Element::Text(instruction.trim().to_string()));
        } else if !challenge.description.trim().is_empty() {
            elements.push(Element::Text(challenge.description.trim().to_string()));
        }
        let ids = feature_ids(&task.geometries);
        if !ids.is_empty() {
            elements.push(Element::Muted(osm::t_args(
                "panel.features",
                &[("ids", &ids.join(", "))],
            )));
        }
        elements.push(Element::Buttons(vec![
            button("load", &osm::t("panel.load"), ButtonStyle::Primary),
            button("zoom", &osm::t("panel.zoom"), ButtonStyle::Secondary),
        ]));
        elements.push(Element::Divider);
        let has_key = api_key().is_some();
        elements.push(Element::Muted(osm::t(if has_key {
            "panel.mark_hint"
        } else {
            "panel.need_key"
        })));
        let status = |code: i32, style| {
            let mut b = button(
                &format!("status-{code}"),
                &osm::t(&format!("button.status.{code}")),
                style,
            );
            b.enabled = has_key;
            b
        };
        elements.push(Element::Buttons(vec![
            status(1, ButtonStyle::Primary),
            status(5, ButtonStyle::Secondary),
        ]));
        elements.push(Element::Buttons(vec![
            status(2, ButtonStyle::Secondary),
            status(6, ButtonStyle::Secondary),
            status(3, ButtonStyle::Ghost),
        ]));
        if let Some(note) = &open.note {
            elements.push(Element::Muted(note.clone()));
        }
        elements.push(Element::Divider);
        elements.push(Element::Link((
            osm::t("panel.open_site"),
            format!("{SITE}/challenge/{}/task/{}", challenge.id, task.id),
        )));
        if !has_key {
            elements.push(Element::Buttons(vec![button(
                "ask-key",
                &osm::t("panel.add_key"),
                ButtonStyle::Ghost,
            )]));
        }
        Some(Panel {
            title: osm::t("panel.title"),
            elements,
        })
    }

    fn show_tasks(&mut self) -> osm::Result<()> {
        if osm::zoom() < MIN_ZOOM {
            osm::set_status(&osm::t("status_bar.zoom_in"));
            return Ok(());
        }
        let found: Vec<TaskSummary> = osm::get_json(&box_url(&osm::viewport()))?;
        let n = found.len().to_string();
        osm::set_status(&match found.len() {
            0 => osm::t("status_bar.none"),
            1 => osm::t("status_bar.one"),
            c if c >= TASK_LIMIT => osm::t_args("status_bar.first", &[("count", &n)]),
            _ => osm::t_args("status_bar.many", &[("count", &n)]),
        });
        self.tasks = found;
        self.redraw();
        Ok(())
    }

    fn open_task(&mut self, id: i64) -> osm::Result<()> {
        let summary = self.tasks.iter().find(|t| t.id == id).cloned();
        let task: Task = osm::get_json(&format!("{API}/task/{id}"))?;
        let challenge: Challenge = osm::get_json(&format!("{API}/challenge/{}", task.parent))?;
        let at = summary.map(|s| s.point).unwrap_or_else(|| {
            let mut pts = Vec::new();
            coords(&task.geometries, &mut pts);
            let (lng, lat) = pts.first().copied().unwrap_or((0.0, 0.0));
            Point { lat, lng }
        });
        self.open = Some(Open {
            task,
            challenge,
            at,
            note: None,
        });
        self.redraw();
        Ok(())
    }

    fn set_task_status(&mut self, status: Status) -> osm::Result<()> {
        let Some(open) = &mut self.open else {
            return Ok(());
        };
        let Some(key) = api_key() else {
            show_key_panel();
            return Ok(());
        };
        let code = status as i32;
        let url = format!("{API}/task/{}/{code}", open.task.id);
        let resp = osm::http_fetch(&HttpRequest {
            method: HttpMethod::Put,
            url,
            headers: vec![("apiKey".into(), key)],
            body: None,
        })?;
        if (200..300).contains(&resp.status) {
            open.task.status = code;
            let label = status_label(code);
            open.note = Some(osm::t_args("panel.marked", &[("status", &label)]));
            if let Some(t) = self.tasks.iter_mut().find(|t| t.id == open.task.id) {
                t.status = code;
            }
            osm::set_status(&osm::t_args(
                "status_bar.marked",
                &[("id", &open.task.id.to_string()), ("status", &label)],
            ));
        } else {
            let why = match resp.status {
                401 | 403 => osm::t("status_bar.key_refused"),
                s => osm::t_args("status_bar.http", &[("status", &s.to_string())]),
            };
            open.note = Some(why.clone());
            osm::set_status(&why);
        }
        self.redraw();
        Ok(())
    }
}

/// The API-key panel, a section of its own beside the task.
fn show_key_panel() {
    let has_key = api_key().is_some();
    let mut buttons = vec![
        button("save-key", &osm::t("key.save"), ButtonStyle::Primary),
        button("cancel-key", &osm::t("key.cancel"), ButtonStyle::Ghost),
    ];
    if has_key {
        buttons.insert(
            1,
            button("forget-key", &osm::t("key.forget"), ButtonStyle::Danger),
        );
    }
    osm::show_panel(
        KEY_PANEL,
        Some(&Panel {
            title: osm::t("key.title"),
            elements: vec![
                Element::Text(osm::t("key.explain")),
                Element::Link((osm::t("key.find"), format!("{SITE}/user/profile"))),
                Element::Input(TextInput {
                    id: "key".into(),
                    label: osm::t("key.label"),
                    placeholder: osm::t(if has_key {
                        "key.placeholder_replace"
                    } else {
                        "key.placeholder"
                    }),
                    value: String::new(),
                    secret: true,
                }),
                Element::Buttons(buttons),
            ],
        }),
    );
}

impl Extension for MapRoulette {
    fn new() -> Self {
        MapRoulette::default()
    }

    fn run_command(&mut self, id: &str) -> osm::Result<()> {
        match id {
            "show-tasks" => self.show_tasks(),
            "hide-tasks" => {
                self.tasks.clear();
                self.open = None;
                self.redraw();
                Ok(())
            }
            "settings" => {
                show_key_panel();
                Ok(())
            }
            other => Err(format!("unknown command {other}")),
        }
    }

    fn marker_clicked(&mut self, id: &str) -> osm::Result<()> {
        let id: i64 = id.parse().map_err(|_| format!("bad task id {id}"))?;
        self.open_task(id)
    }

    fn panel_button(&mut self, panel: &str, id: &str, inputs: &Inputs) -> osm::Result<()> {
        match (panel, id) {
            (TASK_PANEL, "load") => {
                if let Some(open) = &self.open {
                    osm::download_area(task_area(&open.task.geometries, open.at));
                }
            }
            (TASK_PANEL, "zoom") => {
                if let Some(open) = &self.open {
                    osm::fly_to(
                        LatLon {
                            lat: open.at.lat,
                            lon: open.at.lng,
                        },
                        18.0,
                    );
                }
            }
            (TASK_PANEL, "ask-key") => show_key_panel(),
            (KEY_PANEL, "save-key") => {
                let key = inputs.get("key").unwrap_or("").trim();
                if !key.is_empty() {
                    osm::secret_set(API_KEY, Some(key));
                    osm::set_status(&osm::t("status_bar.key_saved"));
                }
                osm::show_panel(KEY_PANEL, None);
                // The status buttons need the key.
                self.redraw();
            }
            (KEY_PANEL, "forget-key") => {
                osm::secret_set(API_KEY, None);
                osm::storage_set(API_KEY, None);
                osm::set_status(&osm::t("status_bar.key_forgotten"));
                osm::show_panel(KEY_PANEL, None);
                self.redraw();
            }
            (KEY_PANEL, "cancel-key") => osm::show_panel(KEY_PANEL, None),
            (TASK_PANEL, other) => {
                if let Some(code) = other.strip_prefix("status-") {
                    let status = code
                        .parse()
                        .ok()
                        .and_then(Status::from_code)
                        .ok_or_else(|| format!("bad status {code}"))?;
                    return self.set_task_status(status);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn panel_closed(&mut self, panel: &str) {
        if panel == TASK_PANEL {
            // Closing the task deselects its pin.
            self.open = None;
            osm::set_markers(&self.markers());
        }
    }

    fn changeset_tags(&mut self) -> Vec<(String, String)> {
        let Some(open) = &self.open else {
            return Vec::new();
        };
        let mut tags = vec![("hashtags".to_string(), "#maproulette".to_string())];
        let comment = open.challenge.checkin_comment.trim();
        if !comment.is_empty() {
            tags.push(("comment".to_string(), comment.to_string()));
        }
        tags
    }

    fn upload_finished(&mut self, _changesets: &[i64]) {
        if let Some(open) = &mut self.open {
            if open.task.status == Status::Created as i32 {
                open.note = Some(osm::t("panel.uploaded"));
                self.redraw();
            }
        }
    }
}

osm::register_extension!(MapRoulette);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_search_answers_parse() {
        let body = r#"[{"id":317096925,"owner":-1,"title":"node/34642174","parentId":52711,
            "parentName":"Traffic Light Placement West US",
            "point":{"lat":44.9592107,"lng":-93.2765766},"status":0,"priority":0}]"#;
        let tasks: Vec<TaskSummary> = serde_json::from_str(body).unwrap();
        assert_eq!(tasks[0].id, 317096925);
        assert_eq!(tasks[0].challenge_id, 52711);
        assert_eq!(tasks[0].point.lng, -93.2765766);
    }

    #[test]
    fn the_task_area_covers_every_feature_with_a_margin() {
        let g: serde_json::Value = serde_json::from_str(
            r#"{"type":"FeatureCollection","features":[
              {"type":"Feature","geometry":{"type":"LineString","coordinates":[[-93.0,45.0],[-92.9,45.1]]},
               "properties":{"@id":"way/1"}},
              {"type":"Feature","geometry":{"type":"Point","coordinates":[-93.05,45.02]},
               "properties":{"@id":"node/2"}}]}"#,
        )
        .unwrap();
        let b = task_area(&g, Point { lat: 0.0, lng: 0.0 });
        assert!(b.min_lon < -93.05 && b.max_lon > -92.9);
        assert!(b.min_lat < 45.0 && b.max_lat > 45.1);
        assert_eq!(feature_ids(&g), vec!["way/1", "node/2"]);
    }

    #[test]
    fn a_point_task_gets_a_useful_area() {
        let b = task_area(
            &serde_json::Value::Null,
            Point {
                lat: 45.0,
                lng: -93.0,
            },
        );
        assert!(b.max_lat - b.min_lat > 0.0019);
        assert!(b.max_lon - b.min_lon > 0.0029);
    }
}
