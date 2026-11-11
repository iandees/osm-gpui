//! Script ops for OSM notes.
//!
//! ```text
//! show_notes on|off          View > Show Notes
//! load_notes_file PATH       show the notes in a local API response (instead of fetching)
//! open_note ID               open a loaded note in the side panel
//! note_comment TEXT          comment on the open note            (a write)
//! note_close [TEXT]          close the open note                 (a write)
//! note_reopen [TEXT]         reopen the open note                (a write)
//! new_note LAT LON TEXT      create a note                       (a write)
//! assert_notes N             N notes are loaded
//! assert_note_status S       the open note is open|closed, or none is open
//! assert_note_comments N     the open note has N comments
//! ```
//!
//! The four writes only run against the OSM development server (the same
//! guard as scripted uploads, see `runner::upload_guard`). A `#` starts a
//! comment in a script, so it cannot appear in note text.

use super::parser::ParseError;

/// What the note ops ask the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum NoteOp {
    Show(bool),
    LoadFile(String),
    Open(i64),
    Comment(String),
    Close(Option<String>),
    Reopen(Option<String>),
    New { lat: f64, lon: f64, text: String },
}

impl NoteOp {
    /// Whether this op writes to the server.
    pub fn is_write(&self) -> bool {
        matches!(
            self,
            Self::Comment(_) | Self::Close(_) | Self::Reopen(_) | Self::New { .. }
        )
    }

    /// Whether the op starts background work (a write, or fetching notes)
    /// the runner must wait out before the next step.
    pub fn waits(&self) -> bool {
        self.is_write() || matches!(self, Self::Show(true))
    }

    /// The script word for this op.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Show(_) => "show_notes",
            Self::LoadFile(_) => "load_notes_file",
            Self::Open(_) => "open_note",
            Self::Comment(_) => "note_comment",
            Self::Close(_) => "note_close",
            Self::Reopen(_) => "note_reopen",
            Self::New { .. } => "new_note",
        }
    }
}

/// What `assert_note*` expects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteCheck {
    /// This many notes are loaded.
    Count(usize),
    /// The open note's status: `Some(true)` open, `Some(false)` closed,
    /// `None` no note is open.
    Status(Option<bool>),
    /// The open note has this many comments.
    Comments(usize),
    /// No note fetch or write is running (used by the runner to wait).
    Idle,
}

fn err(line_no: usize, msg: impl Into<String>) -> ParseError {
    ParseError {
        line_no,
        message: msg.into(),
    }
}

/// The text after the first `skip` whitespace-separated words of `line`
/// (the op word included in the count).
fn text_after(line: &str, skip: usize) -> &str {
    let mut rest = line.trim_start();
    for _ in 0..skip {
        rest = rest
            .trim_start_matches(|c: char| !c.is_whitespace())
            .trim_start();
    }
    rest.trim()
}

/// Parse a note op line; `None` when `head` is not one of them.
pub fn parse_note_op(
    line_no: usize,
    head: &str,
    line: &str,
    rest: &[&str],
) -> Option<Result<super::Op, ParseError>> {
    use super::Op;
    let op = match head {
        "show_notes" => match rest {
            ["on"] => Ok(Op::Note(NoteOp::Show(true))),
            ["off"] => Ok(Op::Note(NoteOp::Show(false))),
            _ => Err(err(line_no, "show_notes: want on|off")),
        },
        "load_notes_file" => match rest {
            [path] => Ok(Op::Note(NoteOp::LoadFile(path.to_string()))),
            _ => Err(err(line_no, "load_notes_file: want PATH")),
        },
        "open_note" => match rest {
            [id] => id
                .parse::<i64>()
                .map(|id| Op::Note(NoteOp::Open(id)))
                .map_err(|e| err(line_no, format!("open_note: bad id '{id}': {e}"))),
            _ => Err(err(line_no, "open_note: want ID")),
        },
        "note_comment" => {
            let text = text_after(line, 1);
            if text.is_empty() {
                Err(err(line_no, "note_comment: want TEXT"))
            } else {
                Ok(Op::Note(NoteOp::Comment(text.to_string())))
            }
        }
        "note_close" | "note_reopen" => {
            let text = text_after(line, 1);
            let text = (!text.is_empty()).then(|| text.to_string());
            Ok(Op::Note(if head == "note_close" {
                NoteOp::Close(text)
            } else {
                NoteOp::Reopen(text)
            }))
        }
        "new_note" => {
            let text = text_after(line, 3);
            match (rest.first(), rest.get(1)) {
                (Some(lat), Some(lon)) if !text.is_empty() => {
                    match (lat.parse::<f64>(), lon.parse::<f64>()) {
                        (Ok(lat), Ok(lon))
                            if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) =>
                        {
                            Ok(Op::Note(NoteOp::New {
                                lat,
                                lon,
                                text: text.to_string(),
                            }))
                        }
                        _ => Err(err(line_no, "new_note: LAT must be -90..90, LON -180..180")),
                    }
                }
                _ => Err(err(line_no, "new_note: want LAT LON TEXT")),
            }
        }
        "assert_notes" => match rest {
            [n] => n
                .parse::<usize>()
                .map(|n| Op::AssertNote(NoteCheck::Count(n)))
                .map_err(|e| err(line_no, format!("assert_notes: bad count '{n}': {e}"))),
            _ => Err(err(line_no, "assert_notes: want COUNT")),
        },
        "assert_note_status" => match rest {
            ["open"] => Ok(Op::AssertNote(NoteCheck::Status(Some(true)))),
            ["closed"] => Ok(Op::AssertNote(NoteCheck::Status(Some(false)))),
            ["none"] => Ok(Op::AssertNote(NoteCheck::Status(None))),
            _ => Err(err(line_no, "assert_note_status: want open|closed|none")),
        },
        "assert_note_comments" => match rest {
            [n] => n
                .parse::<usize>()
                .map(|n| Op::AssertNote(NoteCheck::Comments(n)))
                .map_err(|e| {
                    err(
                        line_no,
                        format!("assert_note_comments: bad count '{n}': {e}"),
                    )
                }),
            _ => Err(err(line_no, "assert_note_comments: want COUNT")),
        },
        _ => return None,
    };
    Some(op)
}

#[cfg(test)]
mod tests {
    use super::super::{parse, Op};
    use super::*;

    fn one(src: &str) -> Op {
        parse(src).unwrap().remove(0).op
    }

    #[test]
    fn parses_every_note_op() {
        assert_eq!(one("show_notes on"), Op::Note(NoteOp::Show(true)));
        assert_eq!(one("show_notes off"), Op::Note(NoteOp::Show(false)));
        assert_eq!(
            one("load_notes_file /tmp/n.xml"),
            Op::Note(NoteOp::LoadFile("/tmp/n.xml".into()))
        );
        assert_eq!(one("open_note 101"), Op::Note(NoteOp::Open(101)));
        assert_eq!(
            one("note_comment still   there & more"),
            Op::Note(NoteOp::Comment("still   there & more".into()))
        );
        assert_eq!(one("note_close"), Op::Note(NoteOp::Close(None)));
        assert_eq!(
            one("note_close all fixed"),
            Op::Note(NoteOp::Close(Some("all fixed".into())))
        );
        assert_eq!(one("note_reopen"), Op::Note(NoteOp::Reopen(None)));
        assert_eq!(
            one("new_note -48 -123 Test note, ignore"),
            Op::Note(NoteOp::New {
                lat: -48.0,
                lon: -123.0,
                text: "Test note, ignore".into()
            })
        );
        assert_eq!(one("assert_notes 3"), Op::AssertNote(NoteCheck::Count(3)));
        assert_eq!(
            one("assert_note_status closed"),
            Op::AssertNote(NoteCheck::Status(Some(false)))
        );
        assert_eq!(
            one("assert_note_status none"),
            Op::AssertNote(NoteCheck::Status(None))
        );
        assert_eq!(
            one("assert_note_comments 2"),
            Op::AssertNote(NoteCheck::Comments(2))
        );
    }

    #[test]
    fn bad_note_ops_are_parse_errors() {
        for bad in [
            "show_notes",
            "show_notes maybe",
            "open_note x",
            "note_comment",
            "new_note 1 2",
            "new_note 95 2 text",
            "new_note a b text",
            "assert_notes",
            "assert_note_status ajar",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_the_four_writes_are_writes() {
        assert!(NoteOp::Comment("x".into()).is_write());
        assert!(NoteOp::Close(None).is_write());
        assert!(NoteOp::Reopen(None).is_write());
        assert!(NoteOp::New {
            lat: 0.0,
            lon: 0.0,
            text: "x".into()
        }
        .is_write());
        assert!(!NoteOp::Show(true).is_write());
        assert!(!NoteOp::LoadFile("x".into()).is_write());
        assert!(!NoteOp::Open(1).is_write());
    }
}
