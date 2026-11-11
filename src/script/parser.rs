//! Line-oriented DSL parser for the screenshot script format.

use super::op::*;
use std::time::Duration;

#[derive(Debug)]
pub struct ParseError {
    pub line_no: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line_no, self.message)
    }
}

impl std::error::Error for ParseError {}

pub fn parse(source: &str) -> Result<Vec<Step>, ParseError> {
    let mut steps = Vec::new();
    for (i, raw) in source.lines().enumerate() {
        let line_no = i + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let op = parse_line(line_no, line)?;
        steps.push(Step { line_no, op });
    }
    Ok(steps)
}

fn strip_comment(s: &str) -> &str {
    match s.find('#') {
        Some(i) => &s[..i],
        None => s,
    }
}

fn err(line_no: usize, msg: impl Into<String>) -> ParseError {
    ParseError {
        line_no,
        message: msg.into(),
    }
}

fn parse_line(line_no: usize, line: &str) -> Result<Op, ParseError> {
    let mut tokens = line.split_whitespace();
    let head = tokens
        .next()
        .ok_or_else(|| err(line_no, "empty line reached parser"))?;
    let rest: Vec<&str> = tokens.collect();
    match head {
        "window" => parse_window(line_no, &rest),
        "viewport" => parse_viewport(line_no, &rest),
        "wait_idle" => parse_wait_idle(line_no, &rest),
        "wait" => parse_wait(line_no, &rest),
        "drag" => parse_drag(line_no, &rest),
        "click" => parse_click(line_no, &rest),
        "scroll" => parse_scroll(line_no, &rest),
        "hover" => match rest.as_slice() {
            [at] => Ok(Op::Hover {
                at: parse_point(line_no, at)?,
            }),
            _ => Err(err(line_no, "hover: want X,Y")),
        },
        "key" => parse_key(line_no, &rest),
        "capture" => parse_capture(line_no, &rest),
        "action" => match rest.as_slice() {
            [name] => Ok(Op::Action {
                name: name.to_string(),
            }),
            // `action AddImageryLayer {"id":"EsriWorldImagery"}`: the rest of
            // the line is the action's JSON data, kept in `name` for the
            // harness to split off.
            [name, data @ ..] if data.join(" ").trim_start().starts_with('{') => Ok(Op::Action {
                name: format!("{} {}", name, data.join(" ")),
            }),
            _ => Err(err(
                line_no,
                "action: want ACTION_NAME like UploadToOsm, optionally followed by JSON data",
            )),
        },
        "log" => Ok(Op::Log {
            message: rest.join(" "),
        }),
        "frame_stats" => Ok(Op::FrameStats {
            label: rest.join("_"),
        }),
        "load_osm" => parse_load_osm(line_no, &rest),
        "assert_mode" => parse_assert_mode(line_no, &rest),
        "assert_selected" => parse_assert_selected(line_no, &rest),
        "type" => {
            let text = text_after(line, head, 0);
            if text.is_empty() {
                return Err(err(line_no, "type: want TEXT"));
            }
            Ok(Op::Type {
                text: text.to_string(),
            })
        }
        "set_tag" => {
            let key = rest.first().copied().unwrap_or("");
            let value = text_after(line, head, 1);
            if key.is_empty() || value.is_empty() {
                return Err(err(line_no, "set_tag: want KEY VALUE"));
            }
            Ok(Op::SetTag {
                key: key.to_string(),
                value: value.to_string(),
            })
        }
        "remove_tag" => match rest.as_slice() {
            [key] => Ok(Op::RemoveTag {
                key: key.to_string(),
            }),
            _ => Err(err(line_no, "remove_tag: want KEY")),
        },
        "upload_confirm" => no_args(line_no, "upload_confirm", &rest, Op::UploadConfirm),
        "upload_cancel" => no_args(line_no, "upload_cancel", &rest, Op::UploadCancel),
        "confirm_delete_layer" => no_args(
            line_no,
            "confirm_delete_layer",
            &rest,
            Op::ConfirmDeleteLayer,
        ),
        "cancel_delete_layer" => {
            no_args(line_no, "cancel_delete_layer", &rest, Op::CancelDeleteLayer)
        }
        "set_comment" => {
            let text = text_after(line, head, 0);
            if text.is_empty() {
                return Err(err(line_no, "set_comment: want TEXT"));
            }
            Ok(Op::SetComment {
                text: text.to_string(),
            })
        }
        "resolve_conflict" => parse_resolve_conflict(line_no, &rest),
        "wait_transfer" => Ok(Op::WaitTransfer {
            timeout: optional_duration(line_no, "wait_transfer", &rest, Duration::from_secs(120))?,
        }),
        "capture_while_transferring" => parse_capture_while_transferring(line_no, &rest),
        "assert_pending" => match rest.as_slice() {
            [n] => Ok(Op::AssertPending {
                count: n.parse::<usize>().map_err(|e| {
                    err(line_no, format!("assert_pending: bad count '{}': {}", n, e))
                })?,
            }),
            _ => Err(err(line_no, "assert_pending: want COUNT")),
        },
        "assert_status_contains" => {
            let text = text_after(line, head, 0);
            if text.is_empty() {
                return Err(err(line_no, "assert_status_contains: want TEXT"));
            }
            Ok(Op::AssertStatusContains {
                text: text.to_string(),
            })
        }
        "assert_imagery_offset" => parse_assert_imagery_offset(line_no, &rest),
        "assert_zoom" => parse_assert_zoom(line_no, &rest),
        "assert_adjusting_offset" => match rest.as_slice() {
            ["on"] => Ok(Op::AssertAdjustingOffset { on: true }),
            ["off"] => Ok(Op::AssertAdjustingOffset { on: false }),
            _ => Err(err(line_no, "assert_adjusting_offset: want on|off")),
        },
        "assert_selected_count" => match rest.as_slice() {
            [n] => Ok(Op::AssertSelectedCount {
                count: n.parse::<usize>().map_err(|e| {
                    err(
                        line_no,
                        format!("assert_selected_count: bad count '{}': {}", n, e),
                    )
                })?,
            }),
            _ => Err(err(line_no, "assert_selected_count: want COUNT")),
        },
        "assert_find_preview" => {
            let text = text_after(line, head, 0);
            if text.is_empty() {
                return Err(err(line_no, "assert_find_preview: want TEXT"));
            }
            Ok(Op::AssertFindPreview {
                text: text.to_string(),
            })
        }
        "assert_dialog" => parse_assert_dialog(line_no, &rest),
        "assert_selection" => parse_assert_selection(line_no, &rest),
        "assert_validation" => parse_assert_validation(line_no, &rest),
        "show_finding" => parse_show_finding(line_no, &rest),
        "assert_validation_panel" => parse_assert_validation_panel(line_no, &rest),
        "assert_native_appearance" => {
            let expect = match rest.as_slice() {
                ["system"] => NativeAppearanceWant::System,
                ["light"] => NativeAppearanceWant::Light,
                ["dark"] => NativeAppearanceWant::Dark,
                _ => {
                    return Err(err(
                        line_no,
                        "assert_native_appearance: want system|light|dark",
                    ))
                }
            };
            Ok(Op::AssertNativeAppearance { expect })
        }
        "export_layer" => match rest.as_slice() {
            [path] => Ok(Op::ExportLayer {
                path: path.to_string(),
            }),
            _ => Err(err(line_no, "export_layer: want PATH")),
        },
        "autosave_now" => no_args(line_no, "autosave_now", &rest, Op::AutosaveNow),
        "assert_autosave_exists" => match rest.as_slice() {
            [] => Ok(Op::AssertAutosaveExists { count: None }),
            [n] => Ok(Op::AssertAutosaveExists {
                count: Some(n.parse::<usize>().map_err(|e| {
                    err(
                        line_no,
                        format!("assert_autosave_exists: bad count '{}': {}", n, e),
                    )
                })?),
            }),
            _ => Err(err(line_no, "assert_autosave_exists: want [COUNT]")),
        },
        "restore_autosave" => no_args(line_no, "restore_autosave", &rest, Op::RestoreAutosave),
        "discard_autosave" => no_args(line_no, "discard_autosave", &rest, Op::DiscardAutosave),
        "crash" => no_args(line_no, "crash", &rest, Op::Crash),
        "show_history" => no_args(line_no, "show_history", &rest, Op::ShowHistory),
        "load_history_file" => match rest.as_slice() {
            [path] => Ok(Op::LoadHistoryFile {
                path: path.to_string(),
            }),
            _ => Err(err(line_no, "load_history_file: want PATH")),
        },
        "assert_history_versions" => match rest.as_slice() {
            [n] => Ok(Op::AssertHistoryVersions {
                count: n.parse::<usize>().map_err(|e| {
                    err(
                        line_no,
                        format!("assert_history_versions: bad count '{}': {}", n, e),
                    )
                })?,
            }),
            _ => Err(err(line_no, "assert_history_versions: want COUNT")),
        },
        "sign_in" => no_args(line_no, "sign_in", &rest, Op::SignIn),
        "wait_signed_in" => Ok(Op::WaitSignedIn {
            timeout: optional_duration(line_no, "wait_signed_in", &rest, Duration::from_secs(200))?,
        }),
        "assert_signed_in" => no_args(line_no, "assert_signed_in", &rest, Op::AssertSignedIn),
        "assert_server" => match rest.as_slice() {
            ["dev"] => Ok(Op::AssertServerDev),
            _ => Err(err(line_no, "assert_server: want dev")),
        },
        "pause" => {
            // `pause [DURATION] MESSAGE`: a leading `2s` / `500ms` is the wait.
            let mut duration = Duration::from_secs(2);
            let mut skip = 0;
            if let Some(first) = rest.first() {
                if let Ok(d) = parse_duration(line_no, first) {
                    duration = d;
                    skip = 1;
                }
            }
            let message = text_after(line, head, skip);
            if message.is_empty() {
                return Err(err(line_no, "pause: want [DURATION] MESSAGE"));
            }
            Ok(Op::Pause {
                duration,
                message: message.to_string(),
            })
        }
        other => Err(err(line_no, format!("unknown op '{}'", other))),
    }
}

/// The rest of `line` after its first token and `skip` more whitespace-separated
/// tokens, with inner spacing kept as written. Empty if there is nothing left.
fn text_after<'a>(line: &'a str, head: &str, skip: usize) -> &'a str {
    let mut rest = line[head.len()..].trim_start();
    for _ in 0..skip {
        rest = match rest.split_once(char::is_whitespace) {
            Some((_, tail)) => tail.trim_start(),
            None => "",
        };
    }
    rest.trim_end()
}

fn no_args(line_no: usize, name: &str, rest: &[&str], op: Op) -> Result<Op, ParseError> {
    if rest.is_empty() {
        Ok(op)
    } else {
        Err(err(line_no, format!("{}: takes no arguments", name)))
    }
}

fn optional_duration(
    line_no: usize,
    name: &str,
    rest: &[&str],
    default: Duration,
) -> Result<Duration, ParseError> {
    match rest {
        [] => Ok(default),
        [d] => parse_duration(line_no, d),
        _ => Err(err(line_no, format!("{}: at most one TIMEOUT arg", name))),
    }
}

fn parse_resolve_conflict(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    let choice = match rest {
        ["keep_mine"] => ConflictChoice::KeepMine,
        ["use_theirs"] => ConflictChoice::UseTheirs,
        ["drop_mine"] => ConflictChoice::DropMine,
        ["show"] => ConflictChoice::Show,
        _ => {
            return Err(err(
                line_no,
                "resolve_conflict: want keep_mine|use_theirs|drop_mine|show",
            ))
        }
    };
    Ok(Op::ResolveConflict { choice })
}

fn parse_capture_while_transferring(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    let Some((prefix, opts)) = rest.split_first() else {
        return Err(err(
            line_no,
            "capture_while_transferring: want PREFIX [every=DUR] [timeout=DUR]",
        ));
    };
    let mut every = Duration::from_millis(700);
    let mut timeout = Duration::from_secs(120);
    for kv in opts {
        let (k, v) = kv.split_once('=').ok_or_else(|| {
            err(
                line_no,
                format!("capture_while_transferring: bad kv '{}'", kv),
            )
        })?;
        match k {
            "every" => every = parse_duration(line_no, v)?,
            "timeout" => timeout = parse_duration(line_no, v)?,
            _ => {
                return Err(err(
                    line_no,
                    format!("capture_while_transferring: unknown key '{}'", k),
                ))
            }
        }
    }
    Ok(Op::CaptureWhileTransferring {
        prefix: prefix.to_string(),
        every,
        timeout,
    })
}

fn parse_feature_kind_word(word: &str) -> Option<FeatureKind> {
    match word {
        "node" => Some(FeatureKind::Node),
        "way" => Some(FeatureKind::Way),
        "relation" => Some(FeatureKind::Relation),
        _ => None,
    }
}

/// `assert_validation errors=N warnings=M CHECK_ID=COUNT ...`: at least one
/// pair; `CHECK_ID` is a validator check id such as `crossing_ways`.
fn parse_assert_validation(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    let usage = "assert_validation: want errors=N and/or warnings=N and/or CHECK_ID=COUNT";
    if rest.is_empty() {
        return Err(err(line_no, usage));
    }
    let mut errors = None;
    let mut warnings = None;
    let mut checks = Vec::new();
    for pair in rest {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(err(line_no, usage));
        };
        let count = value.parse::<usize>().map_err(|e| {
            err(
                line_no,
                format!("assert_validation: bad count '{}': {}", value, e),
            )
        })?;
        match key {
            "errors" => errors = Some(count),
            "warnings" => warnings = Some(count),
            id if crate::validator::Check::from_id(id).is_some() => {
                checks.push((id.to_string(), count))
            }
            other => {
                return Err(err(
                    line_no,
                    format!("assert_validation: unknown check '{}'", other),
                ))
            }
        }
    }
    Ok(Op::AssertValidation {
        errors,
        warnings,
        checks,
    })
}

/// `show_finding N` (0 is the first row) or `show_finding CHECK_ID`.
fn parse_show_finding(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    let target = match rest {
        [word] => match word.parse::<usize>() {
            Ok(n) => FindingTarget::Index(n),
            Err(_) if crate::validator::Check::from_id(word).is_some() => {
                FindingTarget::Check(word.to_string())
            }
            Err(_) => {
                return Err(err(
                    line_no,
                    format!(
                        "show_finding: '{}' is neither a row number nor a check id",
                        word
                    ),
                ))
            }
        },
        _ => return Err(err(line_no, "show_finding: want ROW or CHECK_ID")),
    };
    Ok(Op::ShowFinding { target })
}

/// `assert_validation_panel shown|hidden`.
fn parse_assert_validation_panel(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    match rest {
        ["shown"] => Ok(Op::AssertValidationPanel { shown: true }),
        ["hidden"] => Ok(Op::AssertValidationPanel { shown: false }),
        _ => Err(err(
            line_no,
            "assert_validation_panel: want shown or hidden",
        )),
    }
}

fn parse_assert_dialog(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    let usage = "assert_dialog: want none|editing|in_flight|failed|sign_in|notice|delete_layer|restore|find|history|conflict [KIND] [node|way|relation]";
    let Some((head, tail)) = rest.split_first() else {
        return Err(err(line_no, usage));
    };
    let expect = match (*head, tail) {
        ("none", []) => DialogExpect::None,
        ("editing", []) => DialogExpect::Editing,
        ("in_flight", []) => DialogExpect::InFlight,
        ("failed", []) => DialogExpect::Failed,
        ("sign_in", []) => DialogExpect::SignIn,
        ("notice", []) => DialogExpect::Notice,
        ("delete_layer", []) => DialogExpect::DeleteLayer,
        ("restore", []) => DialogExpect::Restore,
        ("find", []) => DialogExpect::Find,
        ("history", []) => DialogExpect::History,
        ("conflict", words) => {
            let mut kind = None;
            let mut object = None;
            for word in words {
                if let Some(k) = parse_feature_kind_word(word) {
                    object = Some(k);
                    continue;
                }
                kind = Some(match *word {
                    "version_mismatch" => ConflictKindName::VersionMismatch,
                    "already_deleted" => ConflictKindName::AlreadyDeleted,
                    "still_in_use" => ConflictKindName::StillInUse,
                    "missing_dependency" => ConflictKindName::MissingDependency,
                    "other" => ConflictKindName::Other,
                    _ => {
                        return Err(err(
                            line_no,
                            format!("assert_dialog: unknown conflict word '{}'", word),
                        ))
                    }
                });
            }
            DialogExpect::Conflict { kind, object }
        }
        _ => return Err(err(line_no, usage)),
    };
    Ok(Op::AssertDialog { expect })
}

fn parse_assert_selection(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    let kind = match rest {
        ["none"] => SelectionKind::None,
        ["node"] => SelectionKind::Node,
        ["way"] => SelectionKind::Way,
        ["relation"] => SelectionKind::Relation,
        ["multiple"] => SelectionKind::Multiple,
        _ => {
            return Err(err(
                line_no,
                "assert_selection: want none|node|way|relation|multiple",
            ))
        }
    };
    Ok(Op::AssertSelection { kind })
}

/// `assert_imagery_offset EAST NORTH [tol=METRES]` (default tolerance 0.05 m).
fn parse_assert_imagery_offset(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    let usage = "assert_imagery_offset: want EAST NORTH [tol=METRES]";
    let (east, north, tolerance) = match rest {
        [e, n] => (e, n, None),
        [e, n, tol] => (e, n, tol.strip_prefix("tol=")),
        _ => return Err(err(line_no, usage)),
    };
    if rest.len() == 3 && tolerance.is_none() {
        return Err(err(line_no, usage));
    }
    Ok(Op::AssertImageryOffset {
        east: parse_f64(line_no, east, "assert_imagery_offset east")?,
        north: parse_f64(line_no, north, "assert_imagery_offset north")?,
        tolerance: match tolerance {
            Some(t) => parse_f64(line_no, t, "assert_imagery_offset tol")?,
            None => 0.05,
        },
    })
}

/// `assert_zoom LEVEL [tol=N]` (default tolerance 0.001 zoom levels).
fn parse_assert_zoom(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    let usage = "assert_zoom: want LEVEL [tol=N]";
    let (level, tolerance) = match rest {
        [l] => (l, None),
        [l, tol] => (
            l,
            Some(
                tol.strip_prefix("tol=")
                    .ok_or_else(|| err(line_no, usage))?,
            ),
        ),
        _ => return Err(err(line_no, usage)),
    };
    Ok(Op::AssertZoom {
        level: parse_f64(line_no, level, "assert_zoom level")?,
        tolerance: match tolerance {
            Some(t) => parse_f64(line_no, t, "assert_zoom tol")?,
            None => 0.001,
        },
    })
}

fn parse_u32(line_no: usize, s: &str, field: &str) -> Result<u32, ParseError> {
    s.parse::<u32>()
        .map_err(|e| err(line_no, format!("{}: {}", field, e)))
}
fn parse_f32(line_no: usize, s: &str, field: &str) -> Result<f32, ParseError> {
    s.parse::<f32>()
        .map_err(|e| err(line_no, format!("{}: {}", field, e)))
}
fn parse_f64(line_no: usize, s: &str, field: &str) -> Result<f64, ParseError> {
    s.parse::<f64>()
        .map_err(|e| err(line_no, format!("{}: {}", field, e)))
}

fn parse_point(line_no: usize, s: &str) -> Result<Point2, ParseError> {
    let (x, y) = s
        .split_once(',')
        .ok_or_else(|| err(line_no, format!("bad point '{}': want X,Y", s)))?;
    Ok(Point2 {
        x: parse_f32(line_no, x, "point.x")?,
        y: parse_f32(line_no, y, "point.y")?,
    })
}

fn parse_duration(line_no: usize, s: &str) -> Result<Duration, ParseError> {
    if let Some(ms) = s.strip_suffix("ms") {
        let n: u64 = ms
            .parse()
            .map_err(|e| err(line_no, format!("bad ms: {}", e)))?;
        Ok(Duration::from_millis(n))
    } else if let Some(sec) = s.strip_suffix('s') {
        let n: f64 = sec
            .parse()
            .map_err(|e| err(line_no, format!("bad seconds: {}", e)))?;
        Ok(Duration::from_secs_f64(n))
    } else {
        Err(err(
            line_no,
            format!("bad duration '{}': want Nms or Ns", s),
        ))
    }
}

fn parse_window(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() != 2 {
        return Err(err(line_no, "window: want W H"));
    }
    Ok(Op::Window {
        w: parse_u32(line_no, rest[0], "window.w")?,
        h: parse_u32(line_no, rest[1], "window.h")?,
    })
}

fn parse_viewport(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() != 3 {
        return Err(err(line_no, "viewport: want LAT LON ZOOM"));
    }
    Ok(Op::Viewport {
        lat: parse_f64(line_no, rest[0], "viewport.lat")?,
        lon: parse_f64(line_no, rest[1], "viewport.lon")?,
        zoom: parse_f32(line_no, rest[2], "viewport.zoom")?,
    })
}

fn parse_wait_idle(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    let timeout = match rest.len() {
        0 => Duration::from_secs(10),
        1 => parse_duration(line_no, rest[0])?,
        _ => return Err(err(line_no, "wait_idle: at most one TIMEOUT arg")),
    };
    Ok(Op::WaitIdle { timeout })
}

fn parse_wait(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() != 1 {
        return Err(err(line_no, "wait: want DURATION"));
    }
    Ok(Op::Wait {
        duration: parse_duration(line_no, rest[0])?,
    })
}

fn parse_drag(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() < 2 {
        return Err(err(
            line_no,
            "drag: want X1,Y1 X2,Y2 [duration=Nms] [button=left|pan] [alt=true] [cmd=true]",
        ));
    }
    let from = parse_point(line_no, rest[0])?;
    let to = parse_point(line_no, rest[1])?;
    let mut duration = Duration::from_millis(200);
    let mut left = false;
    let mut mods = DragMods::default();
    for kv in &rest[2..] {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| err(line_no, format!("drag: bad kv '{}'", kv)))?;
        match k {
            "duration" => duration = parse_duration(line_no, v)?,
            "button" => match v {
                "left" => left = true,
                "pan" => left = false,
                _ => return Err(err(line_no, format!("drag: bad button '{}'", v))),
            },
            "alt" | "cmd" => {
                let on = match v {
                    "true" => true,
                    "false" => false,
                    _ => return Err(err(line_no, format!("drag: bad {} '{}'", k, v))),
                };
                if k == "alt" {
                    mods.alt = on;
                } else {
                    mods.cmd = on;
                }
            }
            _ => return Err(err(line_no, format!("drag: unknown key '{}'", k))),
        }
    }
    Ok(Op::Drag {
        from,
        to,
        duration,
        left,
        mods,
    })
}

fn parse_click(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.is_empty() {
        return Err(err(
            line_no,
            "click: want X,Y [button=left|right] [count=N] [ctrl=true]",
        ));
    }
    let at = parse_point(line_no, rest[0])?;
    let mut button = MouseButton::Left;
    let mut count: u8 = 1;
    let mut ctrl = false;
    for kv in &rest[1..] {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| err(line_no, format!("click: bad kv '{}'", kv)))?;
        match (k, v) {
            ("button", "left") => button = MouseButton::Left,
            ("button", "right") => button = MouseButton::Right,
            ("count", n) => {
                count = n
                    .parse::<u8>()
                    .map_err(|e| err(line_no, format!("click: bad count '{}': {}", n, e)))?;
            }
            ("ctrl", "true") => ctrl = true,
            ("ctrl", "false") => ctrl = false,
            _ => return Err(err(line_no, format!("click: unknown {}={}", k, v))),
        }
    }
    Ok(Op::Click {
        at,
        button,
        count,
        ctrl,
    })
}

fn parse_scroll(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.is_empty() {
        return Err(err(line_no, "scroll: want X,Y [dx=N] [dy=N]"));
    }
    let at = parse_point(line_no, rest[0])?;
    let mut dx = 0.0f32;
    let mut dy = 0.0f32;
    for kv in &rest[1..] {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| err(line_no, format!("scroll: bad kv '{}'", kv)))?;
        match k {
            "dx" => dx = parse_f32(line_no, v, "scroll.dx")?,
            "dy" => dy = parse_f32(line_no, v, "scroll.dy")?,
            _ => return Err(err(line_no, format!("scroll: unknown key '{}'", k))),
        }
    }
    Ok(Op::Scroll { at, dx, dy })
}

fn parse_key(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() != 1 {
        return Err(err(line_no, "key: want CHORD like cmd+shift+a"));
    }
    let mut chord = Chord {
        cmd: false,
        shift: false,
        alt: false,
        ctrl: false,
        key: String::new(),
    };
    for part in rest[0].split('+') {
        match part {
            "cmd" => chord.cmd = true,
            "shift" => chord.shift = true,
            "alt" => chord.alt = true,
            "ctrl" => chord.ctrl = true,
            k if !k.is_empty() && chord.key.is_empty() => chord.key = k.to_string(),
            k => return Err(err(line_no, format!("key: unexpected '{}'", k))),
        }
    }
    if chord.key.is_empty() {
        return Err(err(line_no, "key: missing base key after modifiers"));
    }
    Ok(Op::Key { chord })
}

fn parse_capture(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() != 1 {
        return Err(err(line_no, "capture: want PATH"));
    }
    Ok(Op::Capture {
        path: rest[0].to_string(),
    })
}

fn parse_load_osm(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() != 1 {
        return Err(err(line_no, "load_osm: want PATH"));
    }
    Ok(Op::LoadOsm {
        path: rest[0].to_string(),
    })
}

fn parse_assert_mode(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() != 1 {
        return Err(err(
            line_no,
            "assert_mode: want select|add|building|extrude",
        ));
    }
    let mode = match rest[0] {
        "select" => EditMode::Select,
        "add" => EditMode::Add,
        "building" => EditMode::Building,
        "extrude" => EditMode::Extrude,
        other => {
            return Err(err(
                line_no,
                format!("assert_mode: unknown mode '{}'", other),
            ))
        }
    };
    Ok(Op::AssertMode { mode })
}

fn parse_assert_selected(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() == 1 && rest[0] == "none" {
        return Ok(Op::AssertSelected { feature: None });
    }
    if rest.len() == 2 {
        let kind = match rest[0] {
            "node" => FeatureKind::Node,
            "way" => FeatureKind::Way,
            "relation" => FeatureKind::Relation,
            other => {
                return Err(err(
                    line_no,
                    format!("assert_selected: unknown kind '{}'", other),
                ))
            }
        };
        let id = rest[1].parse::<i64>().map_err(|e| {
            err(
                line_no,
                format!("assert_selected: bad id '{}': {}", rest[1], e),
            )
        })?;
        return Ok(Op::AssertSelected {
            feature: Some((kind, id)),
        });
    }
    Err(err(
        line_no,
        "assert_selected: want 'none' or 'node|way ID'",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_blanks_and_comments() {
        let s = "\n# hi\nwindow 10 20\n";
        let steps = parse(s).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].op, Op::Window { w: 10, h: 20 });
        assert_eq!(steps[0].line_no, 3);
    }

    #[test]
    fn window_parses() {
        assert_eq!(
            parse("window 1200 800").unwrap()[0].op,
            Op::Window { w: 1200, h: 800 }
        );
    }

    #[test]
    fn window_rejects_bad_args() {
        assert!(parse("window 1200").is_err());
        assert!(parse("window x y").is_err());
    }

    #[test]
    fn viewport_parses() {
        let s = parse("viewport 47.6 -122.3 12").unwrap();
        assert_eq!(
            s[0].op,
            Op::Viewport {
                lat: 47.6,
                lon: -122.3,
                zoom: 12.0
            }
        );
    }

    #[test]
    fn wait_idle_default_and_custom() {
        assert_eq!(
            parse("wait_idle").unwrap()[0].op,
            Op::WaitIdle {
                timeout: Duration::from_secs(10)
            }
        );
        assert_eq!(
            parse("wait_idle 3s").unwrap()[0].op,
            Op::WaitIdle {
                timeout: Duration::from_secs(3)
            }
        );
        assert_eq!(
            parse("wait_idle 500ms").unwrap()[0].op,
            Op::WaitIdle {
                timeout: Duration::from_millis(500)
            }
        );
    }

    #[test]
    fn wait_requires_duration() {
        assert!(parse("wait").is_err());
        assert_eq!(
            parse("wait 2s").unwrap()[0].op,
            Op::Wait {
                duration: Duration::from_secs(2)
            }
        );
    }

    #[test]
    fn drag_parses_with_default_duration() {
        let s = parse("drag 10,20 30,40").unwrap();
        assert_eq!(
            s[0].op,
            Op::Drag {
                from: Point2 { x: 10.0, y: 20.0 },
                to: Point2 { x: 30.0, y: 40.0 },
                duration: Duration::from_millis(200),
                left: false,
                mods: DragMods::default(),
            }
        );
    }

    #[test]
    fn drag_modifiers_parse() {
        let s = parse("drag 1,2 3,4 button=left alt=true cmd=true").unwrap();
        assert!(matches!(
            s[0].op,
            Op::Drag {
                mods: DragMods {
                    alt: true,
                    cmd: true
                },
                ..
            }
        ));
        assert!(parse("drag 1,2 3,4 alt=yes").is_err());
    }

    #[test]
    fn drag_button_left_parses() {
        let s = parse("drag 10,20 30,40 button=left").unwrap();
        assert!(matches!(s[0].op, Op::Drag { left: true, .. }));
        assert!(parse("drag 10,20 30,40 button=middle").is_err());
    }

    #[test]
    fn drag_parses_with_duration_override() {
        let s = parse("drag 0,0 100,0 duration=500ms").unwrap();
        if let Op::Drag { duration, .. } = &s[0].op {
            assert_eq!(*duration, Duration::from_millis(500));
        } else {
            panic!("expected Drag");
        }
    }

    #[test]
    fn click_defaults_left() {
        let s = parse("click 5,6").unwrap();
        assert_eq!(
            s[0].op,
            Op::Click {
                at: Point2 { x: 5.0, y: 6.0 },
                button: MouseButton::Left,
                count: 1,
                ctrl: false,
            }
        );
    }

    #[test]
    fn click_button_right() {
        let s = parse("click 5,6 button=right").unwrap();
        assert_eq!(
            s[0].op,
            Op::Click {
                at: Point2 { x: 5.0, y: 6.0 },
                button: MouseButton::Right,
                count: 1,
                ctrl: false,
            }
        );
    }

    #[test]
    fn click_count_parses() {
        let s = parse("click 5,6 count=2").unwrap();
        assert_eq!(
            s[0].op,
            Op::Click {
                at: Point2 { x: 5.0, y: 6.0 },
                button: MouseButton::Left,
                count: 2,
                ctrl: false,
            }
        );
    }

    #[test]
    fn click_parses_ctrl_modifier() {
        let steps = parse("click 10,20 ctrl=true\n").unwrap();
        assert_eq!(steps.len(), 1);
        match &steps[0].op {
            Op::Click { at, ctrl, .. } => {
                assert_eq!(*at, Point2 { x: 10.0, y: 20.0 });
                assert!(*ctrl);
            }
            other => panic!("expected Op::Click, got {:?}", other),
        }
    }

    #[test]
    fn click_defaults_ctrl_to_false() {
        let steps = parse("click 10,20\n").unwrap();
        match &steps[0].op {
            Op::Click { ctrl, .. } => assert!(!*ctrl),
            other => panic!("expected Op::Click, got {:?}", other),
        }
    }

    #[test]
    fn assert_native_appearance_parses() {
        for (word, want) in [
            ("system", NativeAppearanceWant::System),
            ("light", NativeAppearanceWant::Light),
            ("dark", NativeAppearanceWant::Dark),
        ] {
            assert_eq!(
                parse(&format!("assert_native_appearance {word}")).unwrap()[0].op,
                Op::AssertNativeAppearance { expect: want }
            );
        }
        assert!(parse("assert_native_appearance").is_err());
        assert!(parse("assert_native_appearance blue").is_err());
    }

    #[test]
    fn assert_selected_none_parses() {
        assert_eq!(
            parse("assert_selected none").unwrap()[0].op,
            Op::AssertSelected { feature: None }
        );
    }

    #[test]
    fn assert_selected_way_parses() {
        assert_eq!(
            parse("assert_selected way 10").unwrap()[0].op,
            Op::AssertSelected {
                feature: Some((FeatureKind::Way, 10))
            }
        );
    }

    #[test]
    fn assert_selected_node_parses() {
        assert_eq!(
            parse("assert_selected node 7").unwrap()[0].op,
            Op::AssertSelected {
                feature: Some((FeatureKind::Node, 7))
            }
        );
    }

    #[test]
    fn assert_selected_relation_parses() {
        assert_eq!(
            parse("assert_selected relation 9101").unwrap()[0].op,
            Op::AssertSelected {
                feature: Some((FeatureKind::Relation, 9101))
            }
        );
    }

    #[test]
    fn assert_selected_bad_kind_errors() {
        assert!(parse("assert_selected building 1").is_err());
    }

    #[test]
    fn hover_parses_a_point_and_rejects_anything_else() {
        let s = parse("hover 100,200").unwrap();
        assert!(matches!(s[0].op, Op::Hover { at } if at.x == 100.0 && at.y == 200.0));
        assert!(parse("hover").is_err());
        assert!(parse("hover 1,2 3,4").is_err());
    }

    #[test]
    fn scroll_parses_dx_dy() {
        let s = parse("scroll 100,200 dx=1 dy=-5").unwrap();
        assert_eq!(
            s[0].op,
            Op::Scroll {
                at: Point2 { x: 100.0, y: 200.0 },
                dx: 1.0,
                dy: -5.0
            }
        );
    }

    #[test]
    fn key_parses_chord() {
        let s = parse("key cmd+shift+a").unwrap();
        if let Op::Key { chord } = &s[0].op {
            assert!(chord.cmd && chord.shift && !chord.alt && !chord.ctrl);
            assert_eq!(chord.key, "a");
        } else {
            panic!("expected Key");
        }
    }

    #[test]
    fn action_parses_a_name() {
        let s = parse("action UploadToOsm").unwrap();
        assert_eq!(
            s[0].op,
            Op::Action {
                name: "UploadToOsm".to_string()
            }
        );
        assert!(parse("action").is_err());
        assert!(parse("action A B").is_err());
    }

    #[test]
    fn action_accepts_trailing_json_data() {
        let s = parse(r#"action AddImageryLayer {"id": "X"}"#).unwrap();
        assert_eq!(
            s[0].op,
            Op::Action {
                name: r#"AddImageryLayer {"id": "X"}"#.to_string()
            }
        );
    }

    #[test]
    fn key_requires_base_key() {
        assert!(parse("key cmd+shift").is_err());
    }

    #[test]
    fn unknown_op_errors() {
        let e = parse("wiggle 1 2").unwrap_err();
        assert!(e.message.contains("unknown op"));
    }

    #[test]
    fn capture_captures_path() {
        assert_eq!(
            parse("capture out.png").unwrap()[0].op,
            Op::Capture {
                path: "out.png".into()
            }
        );
    }

    #[test]
    fn frame_stats_takes_a_label() {
        assert_eq!(
            parse("frame_stats mid zoom").unwrap()[0].op,
            Op::FrameStats {
                label: "mid_zoom".into()
            }
        );
    }

    #[test]
    fn log_joins_tokens() {
        assert_eq!(
            parse("log hello world").unwrap()[0].op,
            Op::Log {
                message: "hello world".into()
            }
        );
    }

    #[test]
    fn load_osm_captures_path() {
        assert_eq!(
            parse("load_osm path/to/fixture.osm").unwrap()[0].op,
            Op::LoadOsm {
                path: "path/to/fixture.osm".into()
            }
        );
    }

    #[test]
    fn load_osm_requires_path() {
        assert!(parse("load_osm").is_err());
    }

    #[test]
    fn assert_mode_parses_each_variant() {
        assert_eq!(
            parse("assert_mode select").unwrap()[0].op,
            Op::AssertMode {
                mode: EditMode::Select
            }
        );
        assert_eq!(
            parse("assert_mode add").unwrap()[0].op,
            Op::AssertMode {
                mode: EditMode::Add
            }
        );
        assert_eq!(
            parse("assert_mode building").unwrap()[0].op,
            Op::AssertMode {
                mode: EditMode::Building
            }
        );
        assert_eq!(
            parse("assert_mode extrude").unwrap()[0].op,
            Op::AssertMode {
                mode: EditMode::Extrude
            }
        );
    }

    #[test]
    fn assert_mode_rejects_unknown_mode() {
        let e = parse("assert_mode sideways").unwrap_err();
        assert!(e.message.contains("unknown mode"));
    }

    #[test]
    fn assert_mode_requires_arg() {
        assert!(parse("assert_mode").is_err());
    }

    #[test]
    fn assert_imagery_offset_parses_with_and_without_tolerance() {
        assert_eq!(
            parse("assert_imagery_offset 3.2 -1.75").unwrap()[0].op,
            Op::AssertImageryOffset {
                east: 3.2,
                north: -1.75,
                tolerance: 0.05
            }
        );
        assert_eq!(
            parse("assert_imagery_offset 0 0 tol=0.2").unwrap()[0].op,
            Op::AssertImageryOffset {
                east: 0.0,
                north: 0.0,
                tolerance: 0.2
            }
        );
        assert!(parse("assert_imagery_offset 1").is_err());
        assert!(parse("assert_imagery_offset 1 2 0.2").is_err());
        assert!(parse("assert_imagery_offset a b").is_err());
    }

    #[test]
    fn assert_zoom_parses_with_and_without_tolerance() {
        assert_eq!(
            parse("assert_zoom 12.05").unwrap()[0].op,
            Op::AssertZoom {
                level: 12.05,
                tolerance: 0.001
            }
        );
        assert_eq!(
            parse("assert_zoom 12 tol=0.1").unwrap()[0].op,
            Op::AssertZoom {
                level: 12.0,
                tolerance: 0.1
            }
        );
        assert!(parse("assert_zoom").is_err());
        assert!(parse("assert_zoom 12 0.1").is_err());
        assert!(parse("assert_zoom x").is_err());
    }

    #[test]
    fn assert_adjusting_offset_wants_on_or_off() {
        assert_eq!(
            parse("assert_adjusting_offset on").unwrap()[0].op,
            Op::AssertAdjustingOffset { on: true }
        );
        assert_eq!(
            parse("assert_adjusting_offset off").unwrap()[0].op,
            Op::AssertAdjustingOffset { on: false }
        );
        assert!(parse("assert_adjusting_offset").is_err());
    }

    #[test]
    fn error_reports_line_number() {
        let e = parse("\nwindow 1200\n").unwrap_err();
        assert_eq!(e.line_no, 2);
    }
}
