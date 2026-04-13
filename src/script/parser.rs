//! Line-oriented DSL parser for the screenshot script format.

use std::time::Duration;
use super::op::*;

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
    ParseError { line_no, message: msg.into() }
}

fn parse_line(line_no: usize, line: &str) -> Result<Op, ParseError> {
    let mut tokens = line.split_whitespace();
    let head = tokens.next().ok_or_else(|| err(line_no, "empty line reached parser"))?;
    let rest: Vec<&str> = tokens.collect();
    match head {
        "window" => parse_window(line_no, &rest),
        "viewport" => parse_viewport(line_no, &rest),
        "wait_idle" => parse_wait_idle(line_no, &rest),
        "wait" => parse_wait(line_no, &rest),
        "drag" => parse_drag(line_no, &rest),
        "click" => parse_click(line_no, &rest),
        "scroll" => parse_scroll(line_no, &rest),
        "key" => parse_key(line_no, &rest),
        "capture" => parse_capture(line_no, &rest),
        "log" => Ok(Op::Log { message: rest.join(" ") }),
        other => Err(err(line_no, format!("unknown op '{}'", other))),
    }
}

fn parse_u32(line_no: usize, s: &str, field: &str) -> Result<u32, ParseError> {
    s.parse::<u32>().map_err(|e| err(line_no, format!("{}: {}", field, e)))
}
fn parse_f32(line_no: usize, s: &str, field: &str) -> Result<f32, ParseError> {
    s.parse::<f32>().map_err(|e| err(line_no, format!("{}: {}", field, e)))
}
fn parse_f64(line_no: usize, s: &str, field: &str) -> Result<f64, ParseError> {
    s.parse::<f64>().map_err(|e| err(line_no, format!("{}: {}", field, e)))
}

fn parse_point(line_no: usize, s: &str) -> Result<Point2, ParseError> {
    let (x, y) = s.split_once(',').ok_or_else(|| err(line_no, format!("bad point '{}': want X,Y", s)))?;
    Ok(Point2 {
        x: parse_f32(line_no, x, "point.x")?,
        y: parse_f32(line_no, y, "point.y")?,
    })
}

fn parse_duration(line_no: usize, s: &str) -> Result<Duration, ParseError> {
    if let Some(ms) = s.strip_suffix("ms") {
        let n: u64 = ms.parse().map_err(|e| err(line_no, format!("bad ms: {}", e)))?;
        Ok(Duration::from_millis(n))
    } else if let Some(sec) = s.strip_suffix('s') {
        let n: f64 = sec.parse().map_err(|e| err(line_no, format!("bad seconds: {}", e)))?;
        Ok(Duration::from_secs_f64(n))
    } else {
        Err(err(line_no, format!("bad duration '{}': want Nms or Ns", s)))
    }
}

fn parse_window(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() != 2 { return Err(err(line_no, "window: want W H")); }
    Ok(Op::Window {
        w: parse_u32(line_no, rest[0], "window.w")?,
        h: parse_u32(line_no, rest[1], "window.h")?,
    })
}

fn parse_viewport(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() != 3 { return Err(err(line_no, "viewport: want LAT LON ZOOM")); }
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
    if rest.len() != 1 { return Err(err(line_no, "wait: want DURATION")); }
    Ok(Op::Wait { duration: parse_duration(line_no, rest[0])? })
}

fn parse_drag(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() < 2 { return Err(err(line_no, "drag: want X1,Y1 X2,Y2 [duration=Nms]")); }
    let from = parse_point(line_no, rest[0])?;
    let to = parse_point(line_no, rest[1])?;
    let mut duration = Duration::from_millis(200);
    for kv in &rest[2..] {
        let (k, v) = kv.split_once('=').ok_or_else(|| err(line_no, format!("drag: bad kv '{}'", kv)))?;
        match k {
            "duration" => duration = parse_duration(line_no, v)?,
            _ => return Err(err(line_no, format!("drag: unknown key '{}'", k))),
        }
    }
    Ok(Op::Drag { from, to, duration })
}

fn parse_click(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.is_empty() { return Err(err(line_no, "click: want X,Y [button=left|right]")); }
    let at = parse_point(line_no, rest[0])?;
    let mut button = MouseButton::Left;
    for kv in &rest[1..] {
        let (k, v) = kv.split_once('=').ok_or_else(|| err(line_no, format!("click: bad kv '{}'", kv)))?;
        match (k, v) {
            ("button", "left") => button = MouseButton::Left,
            ("button", "right") => button = MouseButton::Right,
            _ => return Err(err(line_no, format!("click: unknown {}={}", k, v))),
        }
    }
    Ok(Op::Click { at, button })
}

fn parse_scroll(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.is_empty() { return Err(err(line_no, "scroll: want X,Y [dx=N] [dy=N]")); }
    let at = parse_point(line_no, rest[0])?;
    let mut dx = 0.0f32;
    let mut dy = 0.0f32;
    for kv in &rest[1..] {
        let (k, v) = kv.split_once('=').ok_or_else(|| err(line_no, format!("scroll: bad kv '{}'", kv)))?;
        match k {
            "dx" => dx = parse_f32(line_no, v, "scroll.dx")?,
            "dy" => dy = parse_f32(line_no, v, "scroll.dy")?,
            _ => return Err(err(line_no, format!("scroll: unknown key '{}'", k))),
        }
    }
    Ok(Op::Scroll { at, dx, dy })
}

fn parse_key(line_no: usize, rest: &[&str]) -> Result<Op, ParseError> {
    if rest.len() != 1 { return Err(err(line_no, "key: want CHORD like cmd+shift+a")); }
    let mut chord = Chord { cmd: false, shift: false, alt: false, ctrl: false, key: String::new() };
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
    if rest.len() != 1 { return Err(err(line_no, "capture: want PATH")); }
    Ok(Op::Capture { path: rest[0].to_string() })
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
        assert_eq!(parse("window 1200 800").unwrap()[0].op, Op::Window { w: 1200, h: 800 });
    }

    #[test]
    fn window_rejects_bad_args() {
        assert!(parse("window 1200").is_err());
        assert!(parse("window x y").is_err());
    }

    #[test]
    fn viewport_parses() {
        let s = parse("viewport 47.6 -122.3 12").unwrap();
        assert_eq!(s[0].op, Op::Viewport { lat: 47.6, lon: -122.3, zoom: 12.0 });
    }

    #[test]
    fn wait_idle_default_and_custom() {
        assert_eq!(parse("wait_idle").unwrap()[0].op, Op::WaitIdle { timeout: Duration::from_secs(10) });
        assert_eq!(parse("wait_idle 3s").unwrap()[0].op, Op::WaitIdle { timeout: Duration::from_secs(3) });
        assert_eq!(parse("wait_idle 500ms").unwrap()[0].op, Op::WaitIdle { timeout: Duration::from_millis(500) });
    }

    #[test]
    fn wait_requires_duration() {
        assert!(parse("wait").is_err());
        assert_eq!(parse("wait 2s").unwrap()[0].op, Op::Wait { duration: Duration::from_secs(2) });
    }

    #[test]
    fn drag_parses_with_default_duration() {
        let s = parse("drag 10,20 30,40").unwrap();
        assert_eq!(s[0].op, Op::Drag {
            from: Point2 { x: 10.0, y: 20.0 },
            to: Point2 { x: 30.0, y: 40.0 },
            duration: Duration::from_millis(200),
        });
    }

    #[test]
    fn drag_parses_with_duration_override() {
        let s = parse("drag 0,0 100,0 duration=500ms").unwrap();
        if let Op::Drag { duration, .. } = &s[0].op {
            assert_eq!(*duration, Duration::from_millis(500));
        } else { panic!("expected Drag"); }
    }

    #[test]
    fn click_defaults_left() {
        let s = parse("click 5,6").unwrap();
        assert_eq!(s[0].op, Op::Click { at: Point2 { x: 5.0, y: 6.0 }, button: MouseButton::Left });
    }

    #[test]
    fn click_button_right() {
        let s = parse("click 5,6 button=right").unwrap();
        assert_eq!(s[0].op, Op::Click { at: Point2 { x: 5.0, y: 6.0 }, button: MouseButton::Right });
    }

    #[test]
    fn scroll_parses_dx_dy() {
        let s = parse("scroll 100,200 dx=1 dy=-5").unwrap();
        assert_eq!(s[0].op, Op::Scroll { at: Point2 { x: 100.0, y: 200.0 }, dx: 1.0, dy: -5.0 });
    }

    #[test]
    fn key_parses_chord() {
        let s = parse("key cmd+shift+a").unwrap();
        if let Op::Key { chord } = &s[0].op {
            assert!(chord.cmd && chord.shift && !chord.alt && !chord.ctrl);
            assert_eq!(chord.key, "a");
        } else { panic!("expected Key"); }
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
        assert_eq!(parse("capture out.png").unwrap()[0].op, Op::Capture { path: "out.png".into() });
    }

    #[test]
    fn log_joins_tokens() {
        assert_eq!(parse("log hello world").unwrap()[0].op, Op::Log { message: "hello world".into() });
    }

    #[test]
    fn error_reports_line_number() {
        let e = parse("\nwindow 1200\n").unwrap_err();
        assert_eq!(e.line_no, 2);
    }
}
