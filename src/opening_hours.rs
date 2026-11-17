//! A simple model of OSM `opening_hours` values, for the hours editor.
//!
//! The model covers the common shape — groups of days (`Mo`–`Su` plus `PH`,
//! public holidays), each with a list of time ranges or `off` — which is what
//! most shops and amenities use. [`parse_osm`] reads a tag value of that shape
//! (and refuses anything richer, such as month or week selectors, so the
//! editor never silently drops part of a value). [`parse_text`] reads the
//! free-form hours text found on websites ("Monday – Friday: 9 AM – 5 PM").
//! [`Schedule::to_osm`] writes the tag value back.

/// Day codes in model order: the seven weekdays, then public holidays.
pub const DAY_CODES: [&str; 8] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su", "PH"];
/// Index of the public-holiday "day" in a [`Rule::days`] mask.
pub const PH: usize = 7;
/// Minutes in a day; a range ending at midnight ends at `24:00`.
pub const DAY_MINUTES: u16 = 24 * 60;

/// A time range in minutes after midnight. `end` may be `24:00`; an `end`
/// before `start` runs past midnight (`22:00-02:00`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimeRange {
    pub start: u16,
    pub end: u16,
}

impl TimeRange {
    pub fn new(start: u16, end: u16) -> Self {
        Self { start, end }
    }

    fn all_day() -> Self {
        Self::new(0, DAY_MINUTES)
    }
}

/// A group of days sharing the same hours. Empty `times` means closed (`off`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rule {
    pub days: [bool; 8],
    pub times: Vec<TimeRange>,
}

/// The rules of an `opening_hours` value, in order: a later rule replaces an
/// earlier one for the days they share, as in OSM.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Schedule {
    pub rules: Vec<Rule>,
}

type PerDay = [Option<Vec<TimeRange>>; 8];

impl Schedule {
    /// What each day ends up with (`None`: no rule mentions it).
    fn per_day(&self) -> PerDay {
        let mut days: PerDay = Default::default();
        for rule in &self.rules {
            let mut times = rule.times.clone();
            times.sort();
            times.dedup();
            for (d, on) in rule.days.iter().enumerate() {
                if *on {
                    days[d] = Some(times.clone());
                }
            }
        }
        days
    }

    /// The same hours as one rule per distinct set of times, ordered by each
    /// group's first day, so no day is in two rules.
    pub fn normalized(&self) -> Schedule {
        let per_day = self.per_day();
        let mut rules: Vec<Rule> = Vec::new();
        for (d, times) in per_day.iter().enumerate() {
            let Some(times) = times else { continue };
            match rules.iter_mut().find(|r| &r.times == times) {
                Some(rule) => rule.days[d] = true,
                None => {
                    let mut days = [false; 8];
                    days[d] = true;
                    rules.push(Rule {
                        days,
                        times: times.clone(),
                    });
                }
            }
        }
        Schedule { rules }
    }

    /// The OSM tag value: `Mo-Fr 09:00-17:00; Sa 10:00-14:00; Su off`.
    /// Empty when no day has hours.
    pub fn to_osm(&self) -> String {
        let normalized = self.normalized();
        let parts: Vec<String> = normalized
            .rules
            .iter()
            .map(|rule| {
                let every_weekday = rule.days[..PH].iter().all(|d| *d);
                if every_weekday && rule.times == [TimeRange::all_day()] {
                    return "24/7".to_string();
                }
                let times = if rule.times.is_empty() {
                    "off".to_string()
                } else {
                    rule.times
                        .iter()
                        .map(|t| format!("{}-{}", format_time(t.start), format_time(t.end)))
                        .collect::<Vec<_>>()
                        .join(",")
                };
                format!("{} {}", format_days(&rule.days), times)
            })
            .collect();
        parts.join("; ")
    }
}

/// `09:00`; `1440` minutes is `24:00`.
pub fn format_time(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// `Mo-We,Fr,PH`: runs of three or more weekdays as a range, shorter runs
/// listed, public holidays last.
fn format_days(days: &[bool; 8]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut d = 0;
    while d < PH {
        if !days[d] {
            d += 1;
            continue;
        }
        let start = d;
        while d + 1 < PH && days[d + 1] {
            d += 1;
        }
        match d - start {
            0 => parts.push(DAY_CODES[start].to_string()),
            1 => {
                parts.push(DAY_CODES[start].to_string());
                parts.push(DAY_CODES[d].to_string());
            }
            _ => parts.push(format!("{}-{}", DAY_CODES[start], DAY_CODES[d])),
        }
        d += 1;
    }
    if days[PH] {
        parts.push("PH".to_string());
    }
    parts.join(",")
}

/// The days from `a` to `b` inclusive, wrapping past Sunday (`Fr-Mo`).
fn day_range(a: usize, b: usize) -> [bool; 8] {
    let mut days = [false; 8];
    let mut d = a;
    loop {
        days[d] = true;
        if d == b {
            break;
        }
        d = (d + 1) % 7;
    }
    days
}

fn weekdays_all() -> [bool; 8] {
    let mut days = [true; 8];
    days[PH] = false;
    days
}

// ---------------------------------------------------------------------------
// OSM syntax
// ---------------------------------------------------------------------------

/// Read an `opening_hours` value of the shape the editor can show. `None`
/// for anything else (month/week selectors, comments, `||` fallbacks, …).
pub fn parse_osm(value: &str) -> Option<Schedule> {
    let mut rules = Vec::new();
    for part in value.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part == "24/7" {
            rules.push(Rule {
                days: weekdays_all(),
                times: vec![TimeRange::all_day()],
            });
            continue;
        }
        // The selector runs up to the first token that starts the times.
        let split = part
            .char_indices()
            .find(|(i, c)| {
                c.is_ascii_digit()
                    || part[*i..].starts_with("off")
                    || part[*i..].starts_with("closed")
            })
            .map(|(i, _)| i)?;
        let (selector, times) = part.split_at(split);
        let days = parse_osm_days(selector.trim())?;
        let times = parse_osm_times(times.trim())?;
        rules.push(Rule { days, times });
    }
    Some(Schedule { rules })
}

fn day_code_index(code: &str) -> Option<usize> {
    DAY_CODES[..PH].iter().position(|c| *c == code)
}

fn parse_osm_days(selector: &str) -> Option<[bool; 8]> {
    if selector.is_empty() {
        return Some(weekdays_all());
    }
    let mut days = [false; 8];
    for item in selector.split(',') {
        let item = item.trim();
        if item == "PH" {
            days[PH] = true;
        } else if let Some((a, b)) = item.split_once('-') {
            let range = day_range(day_code_index(a.trim())?, day_code_index(b.trim())?);
            for (d, on) in range.iter().enumerate() {
                days[d] |= on;
            }
        } else {
            days[day_code_index(item)?] = true;
        }
    }
    Some(days)
}

fn parse_osm_times(times: &str) -> Option<Vec<TimeRange>> {
    if times == "off" || times == "closed" {
        return Some(Vec::new());
    }
    times
        .split(',')
        .map(|range| {
            let (a, b) = range.trim().split_once('-')?;
            let start = parse_hh_mm(a.trim())?;
            let end = parse_hh_mm(b.trim())?;
            (start < DAY_MINUTES && end <= 2 * DAY_MINUTES && start != end)
                .then_some(TimeRange::new(start, end))
        })
        .collect()
}

/// `HH:MM`, exactly as OSM writes it.
fn parse_hh_mm(s: &str) -> Option<u16> {
    let (h, m) = s.split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    let h: u16 = h.parse().ok()?;
    let m: u16 = m.parse().ok()?;
    (m < 60).then_some(h * 60 + m)
}

// ---------------------------------------------------------------------------
// Free-form website text
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Meridiem {
    Am,
    Pm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Clock {
    hour: u16,
    minute: u16,
    meridiem: Option<Meridiem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Days([bool; 8]),
    Dash,
    Comma,
    Break,
    Time(Clock),
    Closed,
    Open24,
}

/// Multi-word phrases rewritten to one word before tokenizing.
const PHRASES: &[(&str, &str)] = &[
    ("open 24 hours", " open24 "),
    ("24 hours", " open24 "),
    ("24 hrs", " open24 "),
    ("24hrs", " open24 "),
    ("24/7", " open24 "),
    ("7 days a week", " daily "),
    ("seven days a week", " daily "),
    ("every day", " daily "),
    ("all week", " daily "),
    ("public holidays", " holidays "),
    ("bank holidays", " holidays "),
    ("a.m.", "am"),
    ("p.m.", "pm"),
];

fn word_days(word: &str) -> Option<[bool; 8]> {
    let day = |d: usize| {
        let mut days = [false; 8];
        days[d] = true;
        Some(days)
    };
    let full = [
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ];
    if let Some(d) = full
        .iter()
        .position(|f| *f == word || word.strip_suffix('s') == Some(f))
    {
        return day(d);
    }
    match word {
        "mon" => day(0),
        "tue" | "tues" => day(1),
        "wed" | "weds" => day(2),
        "thu" | "thur" | "thurs" => day(3),
        "fri" => day(4),
        "sat" => day(5),
        "sun" => day(6),
        "daily" | "everyday" => Some(weekdays_all()),
        "weekdays" | "weekday" => Some(day_range(0, 4)),
        "weekends" | "weekend" => Some(day_range(5, 6)),
        "holidays" | "holiday" | "ph" => day(PH),
        _ => None,
    }
}

/// The two-letter OSM-style day codes, only taken as days when a day list
/// or a time follows (so "we are open" is not Wednesday).
fn short_day(word: &str) -> Option<usize> {
    ["mo", "tu", "we", "th", "fr", "sa", "su"]
        .iter()
        .position(|c| *c == word)
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut s = text.to_lowercase();
    for dash in ['–', '—', '‒', '−', '~'] {
        s = s.replace(dash, "-");
    }
    for (from, to) in PHRASES {
        s = s.replace(from, to);
    }
    let chars: Vec<char> = s.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' || c == ';' || c == '|' {
            tokens.push(Token::Break);
            i += 1;
        } else if c == '-' {
            tokens.push(Token::Dash);
            i += 1;
        } else if c == ',' || c == '/' || c == '&' {
            tokens.push(Token::Comma);
            i += 1;
        } else if c.is_ascii_digit() {
            let (clock, next) = read_clock(&chars, i);
            if let Some(clock) = clock {
                tokens.push(Token::Time(clock));
            }
            i = next;
        } else if c.is_alphabetic() {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '\'') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let next = chars[i..].iter().find(|c| **c != ' ' && **c != '\t');
            let token = match word.as_str() {
                "to" | "through" | "thru" | "until" | "till" | "til" => Some(Token::Dash),
                "and" => Some(Token::Comma),
                "closed" | "off" => Some(Token::Closed),
                "open24" => Some(Token::Open24),
                "noon" => Some(Token::Time(Clock {
                    hour: 12,
                    minute: 0,
                    meridiem: Some(Meridiem::Pm),
                })),
                "midnight" => Some(Token::Time(Clock {
                    hour: 12,
                    minute: 0,
                    meridiem: Some(Meridiem::Am),
                })),
                w => word_days(w).map(Token::Days).or_else(|| {
                    let d = short_day(w)?;
                    let followed = next
                        .is_none_or(|n| matches!(n, '-' | ',' | ':' | '\n') || n.is_ascii_digit());
                    followed.then(|| {
                        let mut days = [false; 8];
                        days[d] = true;
                        Token::Days(days)
                    })
                }),
            };
            tokens.extend(token);
        } else {
            i += 1;
        }
    }
    tokens
}

/// Read a clock time starting at `chars[i]` (a digit): `9`, `9:30`, `9.30`,
/// `9h30`, `0930`, each optionally followed by `am`/`pm`/`a`/`p`. Returns the
/// time (when it is one) and the index after it.
fn read_clock(chars: &[char], mut i: usize) -> (Option<Clock>, usize) {
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < chars.len() && chars[*i].is_ascii_digit() {
            *i += 1;
        }
        chars[start..*i].iter().collect::<String>()
    };
    let first = digits(&mut i);
    let (hour, mut minute) = if first.len() == 4 {
        (
            first[..2].parse().unwrap_or(99),
            first[2..].parse().unwrap_or(99),
        )
    } else {
        (first.parse().unwrap_or(99), 0)
    };
    if first.len() <= 2
        && i + 1 < chars.len()
        && matches!(chars[i], ':' | '.' | 'h')
        && chars[i + 1].is_ascii_digit()
    {
        i += 1;
        minute = digits(&mut i).parse().unwrap_or(99);
    } else if i < chars.len() && chars[i] == 'h' {
        // "9h": nine o'clock.
        let after = chars.get(i + 1);
        if after.is_none_or(|c| !c.is_alphabetic()) {
            i += 1;
        }
    }
    // An am/pm suffix, possibly after a space.
    let mut j = i;
    while j < chars.len() && chars[j] == ' ' {
        j += 1;
    }
    let mut meridiem = None;
    for (word, m) in [
        ("am", Meridiem::Am),
        ("pm", Meridiem::Pm),
        ("a", Meridiem::Am),
        ("p", Meridiem::Pm),
    ] {
        let wc: Vec<char> = word.chars().collect();
        let end = j + wc.len();
        if end <= chars.len()
            && chars[j..end] == wc[..]
            && chars.get(end).is_none_or(|c| !c.is_alphabetic())
        {
            meridiem = Some(m);
            i = end;
            break;
        }
    }
    let valid = match meridiem {
        Some(_) => (1..=12).contains(&hour) && minute < 60,
        None => hour <= 24 && minute < 60 && (hour < 24 || minute == 0),
    };
    let clock = valid.then_some(Clock {
        hour,
        minute,
        meridiem,
    });
    (clock, i)
}

fn hour24(hour: u16, meridiem: Option<Meridiem>) -> u16 {
    match meridiem {
        Some(Meridiem::Am) => hour % 12,
        Some(Meridiem::Pm) => hour % 12 + 12,
        None => hour,
    }
}

/// Resolve a range whose ends may lack am/pm the way people write them:
/// `9-5` is 09:00-17:00, `5-9pm` is 17:00-21:00, `22-2` runs past midnight.
fn resolve_range(a: Clock, b: Clock) -> Option<TimeRange> {
    let mut sh = hour24(a.hour, a.meridiem);
    let mut eh = hour24(b.hour, b.meridiem);
    match (a.meridiem, b.meridiem) {
        (None, Some(Meridiem::Pm)) if sh < 12 && sh + 12 <= eh => sh += 12,
        (None, Some(Meridiem::Am)) if sh < 12 && sh > eh && eh != 0 => sh += 12,
        (_, None) if b.hour <= 12 && eh <= sh && eh + 12 > sh => eh += 12,
        _ => {}
    }
    let start = sh * 60 + a.minute;
    let mut end = eh * 60 + b.minute;
    if end == 0 {
        end = DAY_MINUTES;
    }
    let start = if start == DAY_MINUTES { 0 } else { start };
    (start != end && start < DAY_MINUTES && end <= DAY_MINUTES)
        .then_some(TimeRange::new(start, end))
}

/// What one stretch of tokens says.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Days([bool; 8]),
    /// Time ranges; empty for "closed".
    Times(Vec<TimeRange>),
    Break,
}

/// The segments of `tokens`, and whether any time could not be read as part
/// of a range.
fn segments(tokens: &[Token]) -> (Vec<Segment>, bool) {
    let mut unread = false;
    let mut out: Vec<Segment> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i] {
            Token::Days(mut days) => {
                i += 1;
                // Days joined by commas or dashes are one selector.
                loop {
                    match (tokens.get(i), tokens.get(i + 1)) {
                        (Some(Token::Dash), Some(Token::Days(next))) => {
                            let from = days[..PH].iter().rposition(|d| *d);
                            let to = next[..PH].iter().position(|d| *d);
                            if let (Some(from), Some(to)) = (from, to) {
                                for (d, on) in day_range(from, to).iter().enumerate() {
                                    days[d] |= on;
                                }
                            }
                            for (d, on) in next.iter().enumerate() {
                                days[d] |= on;
                            }
                            i += 2;
                        }
                        (Some(Token::Comma), Some(Token::Days(next))) => {
                            for (d, on) in next.iter().enumerate() {
                                days[d] |= on;
                            }
                            i += 2;
                        }
                        (Some(Token::Days(next)), _) => {
                            for (d, on) in next.iter().enumerate() {
                                days[d] |= on;
                            }
                            i += 1;
                        }
                        _ => break,
                    }
                }
                match out.last_mut() {
                    Some(Segment::Days(prev)) => {
                        for (d, on) in days.iter().enumerate() {
                            prev[d] |= on;
                        }
                    }
                    _ => out.push(Segment::Days(days)),
                }
            }
            Token::Time(a) => {
                let range = match (tokens.get(i + 1), tokens.get(i + 2)) {
                    (Some(Token::Dash), Some(Token::Time(b))) => {
                        i += 3;
                        resolve_range(a, *b)
                    }
                    _ => {
                        i += 1;
                        None
                    }
                };
                // A time that is not one end of a usable range ("10 am to
                // close") means the text says more than was read.
                if range.is_none() {
                    unread = true;
                }
                if let Some(range) = range {
                    match out.last_mut() {
                        Some(Segment::Times(times)) if !times.is_empty() => times.push(range),
                        _ => out.push(Segment::Times(vec![range])),
                    }
                }
            }
            Token::Closed => {
                out.push(Segment::Times(Vec::new()));
                i += 1;
            }
            Token::Open24 => {
                out.push(Segment::Times(vec![TimeRange::all_day()]));
                i += 1;
            }
            Token::Break => {
                if out.last() != Some(&Segment::Break) {
                    out.push(Segment::Break);
                }
                i += 1;
            }
            Token::Dash | Token::Comma => i += 1,
        }
    }
    (out, unread)
}

/// Why [`parse_text`] could not read pasted text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextError {
    /// No days or times at all.
    NothingFound,
    /// Some days or times were found but could not all be paired up (a time
    /// with no end, days with no times, …), so the result would be wrong.
    Partial,
}

/// Read hours text copied from a website ("Mon–Fri 9am–5pm, Sat 10–2,
/// Sun closed"). Refuses text it can only partly read rather than guess.
pub fn parse_text(text: &str) -> Result<Schedule, TextError> {
    let (segs, mut unread) = segments(&tokenize(text));
    let mut rules: Vec<Rule> = Vec::new();
    let mut pending_days: Option<[bool; 8]> = None;
    let mut pending_times: Option<Vec<TimeRange>> = None;
    for seg in segs.into_iter().chain([Segment::Break]) {
        match seg {
            Segment::Days(days) => {
                if let Some(times) = pending_times.take() {
                    // "9am-5pm Mon-Fri": the times came first.
                    rules.push(Rule { days, times });
                } else if let Some(prev) = pending_days.as_mut() {
                    for (d, on) in days.iter().enumerate() {
                        prev[d] |= on;
                    }
                } else {
                    pending_days = Some(days);
                }
            }
            Segment::Times(times) => match pending_days.take() {
                Some(days) => rules.push(Rule { days, times }),
                None => pending_times = Some(times),
            },
            Segment::Break => {
                // Times on a line of their own with no days: every day.
                // (Days on a line of their own wait for the next line's
                // times, as some sites list them.)
                if let Some(times) = pending_times.take() {
                    rules.push(Rule {
                        days: weekdays_all(),
                        times,
                    });
                }
            }
        }
    }
    // Days that never got times ("Open daily 9-5 except Sunday").
    if pending_days.is_some() {
        unread = true;
    }
    match (rules.is_empty(), unread) {
        (true, false) => Err(TextError::NothingFound),
        (_, true) => Err(TextError::Partial),
        (false, false) => Ok(Schedule { rules }.normalized()),
    }
}

/// A time typed into the editor: `9`, `9:30`, `0930`, `17:00`, `5pm`,
/// `24:00`. `None` when it isn't one.
pub fn parse_time(text: &str) -> Option<u16> {
    let s = text.trim().to_lowercase();
    let chars: Vec<char> = s.chars().collect();
    if !chars.first()?.is_ascii_digit() {
        return None;
    }
    let (clock, next) = read_clock(&chars, 0);
    let clock = clock?;
    if next != chars.len() {
        return None;
    }
    Some(hour24(clock.hour, clock.meridiem) * 60 + clock.minute)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> String {
        parse_text(s).map(|s| s.to_osm()).unwrap_or_default()
    }

    fn osm_roundtrip(s: &str) -> String {
        parse_osm(s).expect(s).to_osm()
    }

    #[test]
    fn formats_groups_of_days() {
        assert_eq!(osm_roundtrip("Mo-Fr 09:00-17:00"), "Mo-Fr 09:00-17:00");
        assert_eq!(
            osm_roundtrip("Mo,Tu,We,Fr 09:00-17:00; Sa,Su off"),
            "Mo-We,Fr 09:00-17:00; Sa,Su off"
        );
        assert_eq!(
            osm_roundtrip("Mo-Fr 09:00-12:00,13:00-17:00; PH off"),
            "Mo-Fr 09:00-12:00,13:00-17:00; PH off"
        );
        assert_eq!(osm_roundtrip("Mo-Su 00:00-24:00"), "24/7");
        assert_eq!(osm_roundtrip("24/7"), "24/7");
        assert_eq!(osm_roundtrip("24/7; PH off"), "24/7; PH off");
        assert_eq!(osm_roundtrip("Fr-Mo 18:00-02:00"), "Mo,Fr-Su 18:00-02:00");
        assert_eq!(osm_roundtrip(""), "");
    }

    #[test]
    fn later_osm_rules_override_earlier_days() {
        assert_eq!(
            osm_roundtrip("Mo-Su 08:00-20:00; Su 10:00-16:00"),
            "Mo-Sa 08:00-20:00; Su 10:00-16:00"
        );
        assert_eq!(
            osm_roundtrip("09:00-17:00; Su off"),
            "Mo-Sa 09:00-17:00; Su off"
        );
    }

    #[test]
    fn refuses_osm_it_cannot_show() {
        assert!(parse_osm("Jan-Mar Mo-Fr 09:00-17:00").is_none());
        assert!(parse_osm("Mo-Fr 09:00-17:00 || \"by appointment\"").is_none());
        assert!(parse_osm("sunrise-sunset").is_none());
        assert!(parse_osm("Mo[1] 09:00-12:00").is_none());
        assert!(parse_osm("Mo-Fr 9-5").is_none());
    }

    #[test]
    fn reads_common_website_formats() {
        assert_eq!(
            text(
                "Monday - Friday: 9:00 AM - 5:00 PM\nSaturday: 10:00 AM - 2:00 PM\nSunday: Closed"
            ),
            "Mo-Fr 09:00-17:00; Sa 10:00-14:00; Su off"
        );
        assert_eq!(
            text("Mon–Fri 9am–5pm, Sat 10–4"),
            "Mo-Fr 09:00-17:00; Sa 10:00-16:00"
        );
        assert_eq!(text("Open daily 7am - 10pm"), "Mo-Su 07:00-22:00");
        assert_eq!(text("Open 24 hours"), "24/7");
        assert_eq!(text("24/7"), "24/7");
        assert_eq!(
            text("Weekdays 9-5, weekends 10-6"),
            "Mo-Fr 09:00-17:00; Sa,Su 10:00-18:00"
        );
        assert_eq!(
            text("Mon, Wed & Fri: 8:00-12:00, 13:00-17:00"),
            "Mo,We,Fr 08:00-12:00,13:00-17:00"
        );
        assert_eq!(
            text("Tuesday through Saturday 11 a.m. to 9 p.m."),
            "Tu-Sa 11:00-21:00"
        );
    }

    #[test]
    fn reads_google_style_listing() {
        let listing = "Monday\t9 AM–5 PM\nTuesday\t9 AM–5 PM\nWednesday\t9 AM–5 PM\n\
                       Thursday\t9 AM–8 PM\nFriday\t9 AM–5 PM\nSaturday\t10 AM–2 PM\nSunday\tClosed";
        assert_eq!(
            text(listing),
            "Mo-We,Fr 09:00-17:00; Th 09:00-20:00; Sa 10:00-14:00; Su off"
        );
    }

    #[test]
    fn days_on_their_own_line_take_the_next_lines_times() {
        assert_eq!(text("Monday - Friday\n8am - 6pm"), "Mo-Fr 08:00-18:00");
    }

    #[test]
    fn times_before_days() {
        assert_eq!(
            text("9am-5pm Mon-Fri, 10am-2pm Sat"),
            "Mo-Fr 09:00-17:00; Sa 10:00-14:00"
        );
        assert_eq!(
            text("Daily 9-5; closed Sundays"),
            "Mo-Sa 09:00-17:00; Su off"
        );
    }

    #[test]
    fn am_pm_inference() {
        assert_eq!(text("Mo 5-9pm"), "Mo 17:00-21:00");
        assert_eq!(text("Mo 11-2pm"), "Mo 11:00-14:00");
        assert_eq!(text("Mo 11am-2"), "Mo 11:00-14:00");
        assert_eq!(text("Mo 12-5"), "Mo 12:00-17:00");
        assert_eq!(text("Fri 10pm-2am"), "Fr 22:00-02:00");
        assert_eq!(text("Fri 6pm-midnight"), "Fr 18:00-24:00");
        assert_eq!(text("Fri 11am-noon"), "Fr 11:00-12:00");
        assert_eq!(text("Sat 22:00-02:00"), "Sa 22:00-02:00");
        assert_eq!(text("Sat 0800-1700"), "Sa 08:00-17:00");
        assert_eq!(text("Sat 8h30-17h"), "Sa 08:30-17:00");
    }

    #[test]
    fn two_letter_words_are_not_days_in_prose() {
        assert_eq!(text("We are open Mon-Fri 9-5"), "Mo-Fr 09:00-17:00");
        assert_eq!(text("We 9-5"), "We 09:00-17:00");
    }

    #[test]
    fn holidays() {
        assert_eq!(
            text("Mon-Fri 9-5\nPublic holidays: closed"),
            "Mo-Fr 09:00-17:00; PH off"
        );
    }

    #[test]
    fn nothing_recognised() {
        assert_eq!(
            parse_text("Call us for an appointment"),
            Err(TextError::NothingFound)
        );
        assert_eq!(parse_text(""), Err(TextError::NothingFound));
    }

    #[test]
    fn text_it_can_only_partly_read_is_refused() {
        // "to Close" has no end time; reading the rest would give every day
        // the food hours.
        let text = "Monday-Thursday    10:00 am to Close\n\n\
                    Friday & Saturday    10:00 am to Close\n\n\
                    Sunday  10:00 am to Close\n\n\
                    Food served 11:00 am to 10:00 pm every day";
        assert_eq!(parse_text(text), Err(TextError::Partial));
        // Days that never get times.
        assert_eq!(
            parse_text("Open daily 9-5 except Sunday"),
            Err(TextError::Partial)
        );
    }

    #[test]
    fn parses_typed_times() {
        assert_eq!(parse_time("9"), Some(540));
        assert_eq!(parse_time("09:30"), Some(570));
        assert_eq!(parse_time("5pm"), Some(1020));
        assert_eq!(parse_time("5:30 PM"), Some(1050));
        assert_eq!(parse_time("24:00"), Some(1440));
        assert_eq!(parse_time("0930"), Some(570));
        assert_eq!(parse_time("25:00"), None);
        assert_eq!(parse_time("9:75"), None);
        assert_eq!(parse_time("nine"), None);
        assert_eq!(parse_time("9 to 5"), None);
    }
}
