//! Query text to [`Query`]. A hand-written recursive-descent parser over the
//! characters of the input; every error carries the character position it
//! was found at, and no input makes it panic.

use super::{Expr, Pat, Query, Range, ValueTest};
use crate::selection::FeatureKind;

/// How deep parentheses and prefix operators may nest.
const MAX_DEPTH: usize = 64;

/// Why a query did not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseErrorKind {
    /// The query is blank.
    Empty,
    /// An expression was needed here (after `-`, `OR`, `child`, an opening
    /// parenthesis, ...) but the query ends or something else follows.
    ExpectedExpression,
    /// A `)` with no matching `(`.
    UnexpectedCloseParen,
    /// A `(` that is never closed (the position is the `(`).
    UnclosedParen,
    /// A `"` that is never closed (the position is the opening quote).
    UnterminatedQuote,
    /// `""` on its own, which would match everything.
    EmptyQuotes,
    /// An operator (`=`, `~`, `:`) with no key before it.
    MissingKey,
    /// A number was expected (carries the text found).
    InvalidNumber(String),
    /// A range whose start is above its end (carries the text).
    InvalidRange(String),
    /// `type:` followed by something other than node, way or relation.
    UnknownType(String),
    /// Parentheses or prefix operators nested too deeply.
    TooDeep,
}

/// A parse failure and where in the query it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Character offset (0-based, counted in `char`s, not bytes).
    pub pos: usize,
    pub kind: ParseErrorKind,
}

impl ParseError {
    /// The 1-based column to show a person.
    pub fn column(&self) -> usize {
        self.pos + 1
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use ParseErrorKind::*;
        let what = match &self.kind {
            Empty => "the query is empty".to_string(),
            ExpectedExpression => "expected something to search for".to_string(),
            UnexpectedCloseParen => "unexpected ')'".to_string(),
            UnclosedParen => "this '(' is never closed".to_string(),
            UnterminatedQuote => "this quote is never closed".to_string(),
            EmptyQuotes => "empty quotes".to_string(),
            MissingKey => "a key is needed before this".to_string(),
            InvalidNumber(t) => format!("'{t}' is not a number"),
            InvalidRange(t) => format!("'{t}' is not a valid range"),
            UnknownType(t) => format!("unknown type '{t}' (use node, way or relation)"),
            TooDeep => "nested too deeply".to_string(),
        };
        write!(f, "at position {}: {what}", self.column())
    }
}

impl std::error::Error for ParseError {}

/// Parse `input` into a [`Query`].
pub fn parse(input: &str) -> Result<Query, ParseError> {
    let mut p = Parser {
        chars: input.chars().collect(),
        i: 0,
        structural: 0,
    };
    p.skip_ws();
    if p.i >= p.chars.len() {
        return Err(ParseError {
            pos: 0,
            kind: ParseErrorKind::Empty,
        });
    }
    let expr = p.parse_or(0)?;
    p.skip_ws();
    match p.peek() {
        None => Ok(Query {
            expr,
            structural: p.structural,
        }),
        Some(')') => Err(p.err_here(ParseErrorKind::UnexpectedCloseParen)),
        // parse_or only stops early at `)`.
        Some(_) => Err(p.err_here(ParseErrorKind::ExpectedExpression)),
    }
}

struct Parser {
    chars: Vec<char>,
    i: usize,
    structural: usize,
}

/// A piece of text from the query: its characters and whether it was quoted
/// (quoted text takes `*` literally).
struct Text {
    s: String,
    quoted: bool,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.i + offset).copied()
    }

    fn err_here(&self, kind: ParseErrorKind) -> ParseError {
        ParseError { pos: self.i, kind }
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.i += 1;
        }
    }

    /// Whether the word `kw` starts here as a whole word (followed by white
    /// space, a parenthesis, a quote or the end).
    fn at_keyword(&self, kw: &str) -> bool {
        let n = kw.chars().count();
        if !kw
            .chars()
            .enumerate()
            .all(|(k, c)| self.peek_at(k) == Some(c))
        {
            return false;
        }
        match self.peek_at(n) {
            None => true,
            Some(c) => c.is_whitespace() || c == '(' || c == ')' || c == '"',
        }
    }

    fn at_or(&self) -> bool {
        self.peek() == Some('|') || self.at_keyword("OR")
    }

    fn at_and(&self) -> bool {
        self.peek() == Some('&') || self.at_keyword("AND")
    }

    fn parse_or(&mut self, depth: usize) -> Result<Expr, ParseError> {
        let mut alts = vec![self.parse_and(depth)?];
        loop {
            self.skip_ws();
            if !self.at_or() {
                break;
            }
            if self.peek() == Some('|') {
                while self.peek() == Some('|') {
                    self.i += 1;
                }
            } else {
                self.i += 2;
            }
            alts.push(self.parse_and(depth)?);
        }
        Ok(if alts.len() == 1 {
            alts.pop().expect("one alternative")
        } else {
            Expr::Or(alts)
        })
    }

    fn parse_and(&mut self, depth: usize) -> Result<Expr, ParseError> {
        let mut items: Vec<Expr> = Vec::new();
        loop {
            self.skip_ws();
            if self.at_and() {
                if items.is_empty() {
                    return Err(self.err_here(ParseErrorKind::ExpectedExpression));
                }
                self.i += if self.peek() == Some('&') { 1 } else { 3 };
                self.skip_ws();
                // An operand must follow an explicit AND.
                if self.peek().is_none() || self.peek() == Some(')') || self.at_or() {
                    return Err(self.err_here(ParseErrorKind::ExpectedExpression));
                }
                continue;
            }
            if self.peek().is_none() || self.peek() == Some(')') || self.at_or() {
                break;
            }
            items.push(self.parse_unary(depth)?);
        }
        match items.len() {
            // A `)` where nothing is open.
            0 if depth == 0 && self.peek() == Some(')') => {
                Err(self.err_here(ParseErrorKind::UnexpectedCloseParen))
            }
            0 => Err(self.err_here(ParseErrorKind::ExpectedExpression)),
            1 => Ok(items.pop().expect("one item")),
            _ => Ok(Expr::And(items)),
        }
    }

    fn parse_unary(&mut self, depth: usize) -> Result<Expr, ParseError> {
        if depth >= MAX_DEPTH {
            return Err(self.err_here(ParseErrorKind::TooDeep));
        }
        self.skip_ws();
        match self.peek() {
            None => Err(self.err_here(ParseErrorKind::ExpectedExpression)),
            Some('(') => {
                let open = self.i;
                self.i += 1;
                let inner = self.parse_or(depth + 1)?;
                self.skip_ws();
                if self.peek() == Some(')') {
                    self.i += 1;
                    Ok(inner)
                } else {
                    Err(ParseError {
                        pos: open,
                        kind: ParseErrorKind::UnclosedParen,
                    })
                }
            }
            Some(')') => Err(self.err_here(ParseErrorKind::UnexpectedCloseParen)),
            Some('-') => {
                if self.peek_at(1).is_none_or(char::is_whitespace) {
                    return Err(self.err_here(ParseErrorKind::ExpectedExpression));
                }
                self.i += 1;
                Ok(Expr::Not(Box::new(self.parse_unary(depth + 1)?)))
            }
            Some(_) if self.at_keyword("NOT") => {
                self.i += 3;
                Ok(Expr::Not(Box::new(self.parse_unary(depth + 1)?)))
            }
            Some(_) if self.at_keyword("child") => {
                self.i += 5;
                let expr = self.parse_unary(depth + 1)?;
                let index = self.structural;
                self.structural += 1;
                Ok(Expr::Child {
                    index,
                    expr: Box::new(expr),
                })
            }
            Some(_) if self.at_keyword("parent") => {
                self.i += 6;
                let expr = self.parse_unary(depth + 1)?;
                let index = self.structural;
                self.structural += 1;
                Ok(Expr::Parent {
                    index,
                    expr: Box::new(expr),
                })
            }
            Some(_) => self.parse_term(),
        }
    }

    /// A double-quoted string (the cursor is on the opening quote). `\"` and
    /// `\\` are escapes.
    fn parse_quoted(&mut self) -> Result<Text, ParseError> {
        let open = self.i;
        self.i += 1;
        let mut s = String::new();
        loop {
            match self.peek() {
                None => {
                    return Err(ParseError {
                        pos: open,
                        kind: ParseErrorKind::UnterminatedQuote,
                    })
                }
                Some('"') => {
                    self.i += 1;
                    return Ok(Text { s, quoted: true });
                }
                Some('\\') if matches!(self.peek_at(1), Some('"') | Some('\\')) => {
                    s.push(self.peek_at(1).expect("checked"));
                    self.i += 2;
                }
                Some(c) => {
                    s.push(c);
                    self.i += 1;
                }
            }
        }
    }

    /// An unquoted run up to white space, a parenthesis, `|` or a quote, and
    /// (for keys and words) up to `=` and `~`.
    fn parse_run(&mut self, stop_at_operators: bool) -> String {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_whitespace() || matches!(c, '(' | ')' | '|' | '"') {
                break;
            }
            if stop_at_operators && matches!(c, '=' | '~') {
                break;
            }
            s.push(c);
            self.i += 1;
        }
        s
    }

    fn parse_term(&mut self) -> Result<Expr, ParseError> {
        let start = self.i;
        let key = if self.peek() == Some('"') {
            self.parse_quoted()?
        } else {
            let s = self.parse_run(true);
            if s.is_empty() && !matches!(self.peek(), Some('=') | Some('~')) {
                // A character no term can start with; cannot happen for
                // anything parse_and lets through, but never loop on it.
                return Err(self.err_here(ParseErrorKind::ExpectedExpression));
            }
            Text { s, quoted: false }
        };

        match self.peek() {
            Some(op @ ('=' | '~')) => {
                if key.s.is_empty() {
                    return Err(self.err_here(ParseErrorKind::MissingKey));
                }
                self.i += 1;
                let value = self.parse_value()?;
                Ok(self.tag_with_operator(op, &key, value))
            }
            // `"quoted key":value`
            Some(':') if key.quoted => {
                if key.s.is_empty() {
                    return Err(self.err_here(ParseErrorKind::MissingKey));
                }
                self.i += 1;
                let value = self.parse_value()?;
                Ok(Expr::Tag {
                    key: key_pat(&key),
                    value: contains_test(&value),
                })
            }
            _ if key.quoted => {
                if key.s.is_empty() {
                    return Err(ParseError {
                        pos: start,
                        kind: ParseErrorKind::EmptyQuotes,
                    });
                }
                Ok(Expr::Word(Pat::Contains(key.s.to_lowercase())))
            }
            _ => self.word_or_keyword(start, key.s),
        }
    }

    /// The value after `=`, `~` or a quoted key's `:`: quoted, or an unquoted
    /// run.
    fn parse_value(&mut self) -> Result<Text, ParseError> {
        if self.peek() == Some('"') {
            self.parse_quoted()
        } else {
            Ok(Text {
                s: self.parse_run(false),
                quoted: false,
            })
        }
    }

    fn tag_with_operator(&self, op: char, key: &Text, value: Text) -> Expr {
        let key = key_pat(key);
        if op == '~' {
            // Without a regex engine `~` is a "contains" match in which `*`
            // is a wildcard.
            let value = if value.s.is_empty() {
                ValueTest::Present
            } else {
                ValueTest::Matches(contains_glob(&value))
            };
            return Expr::Tag { key, value };
        }
        let test = if value.s.is_empty() {
            ValueTest::Absent
        } else if !value.quoted && value.s == "*" {
            ValueTest::Present
        } else {
            ValueTest::Matches(exact_pat(&value))
        };
        Expr::Tag { key, value: test }
    }

    /// An unquoted word without `=` / `~`: a state keyword, `special:value`,
    /// `key:value`, or a plain word.
    fn word_or_keyword(&mut self, start: usize, word: String) -> Result<Expr, ParseError> {
        match word.as_str() {
            "modified" => return Ok(Expr::Modified),
            "new" => return Ok(Expr::New),
            "selected" => return Ok(Expr::Selected),
            "untagged" => return Ok(Expr::Untagged),
            "closed" => return Ok(Expr::Closed),
            "incomplete" => return Ok(Expr::Incomplete),
            "inview" => return Ok(Expr::InView),
            _ => {}
        }
        let Some((left, right)) = word.split_once(':') else {
            return Ok(Expr::Word(Pat::Contains(word.to_lowercase())));
        };
        let value_pos = start + left.chars().count() + 1;
        // `key:"quoted value"`: the run stopped at the quote.
        let quoted_value = if right.is_empty() && self.peek() == Some('"') {
            Some(self.parse_quoted()?)
        } else {
            None
        };
        let right_text = quoted_value.as_ref().map_or(right, |t| t.s.as_str());
        let err_at = |kind| ParseError {
            pos: value_pos,
            kind,
        };
        match left.to_ascii_lowercase().as_str() {
            "type" => {
                let kind = match right_text.to_ascii_lowercase().as_str() {
                    "node" => FeatureKind::Node,
                    "way" => FeatureKind::Way,
                    "relation" => FeatureKind::Relation,
                    other => return Err(err_at(ParseErrorKind::UnknownType(other.to_string()))),
                };
                Ok(Expr::Type(kind))
            }
            "id" => right_text
                .parse::<i64>()
                .map(Expr::Id)
                .map_err(|_| err_at(ParseErrorKind::InvalidNumber(right_text.to_string()))),
            "version" | "nodes" | "tags" | "members" => {
                let range = parse_range(right_text).map_err(err_at)?;
                Ok(match left {
                    "version" => Expr::Version(range),
                    "nodes" => Expr::Nodes(range),
                    "tags" => Expr::Tags(range),
                    _ => Expr::Members(range),
                })
            }
            "" => Err(ParseError {
                pos: start,
                kind: ParseErrorKind::MissingKey,
            }),
            _ => {
                let key = Text {
                    s: left.to_string(),
                    quoted: false,
                };
                let value = quoted_value.unwrap_or(Text {
                    s: right.to_string(),
                    quoted: false,
                });
                Ok(Expr::Tag {
                    key: key_pat(&key),
                    value: contains_test(&value),
                })
            }
        }
    }
}

/// `key:` alone means the key is present; otherwise a substring test.
fn contains_test(value: &Text) -> ValueTest {
    if value.s.is_empty() {
        ValueTest::Present
    } else {
        ValueTest::Matches(Pat::Contains(value.s.to_lowercase()))
    }
}

fn has_wildcard(t: &Text) -> bool {
    !t.quoted && t.s.contains('*')
}

fn key_pat(key: &Text) -> Pat {
    if has_wildcard(key) {
        glob(&key.s, false, false)
    } else {
        Pat::Exact(key.s.to_lowercase())
    }
}

fn exact_pat(value: &Text) -> Pat {
    if has_wildcard(value) {
        glob(&value.s, false, false)
    } else {
        Pat::Exact(value.s.to_lowercase())
    }
}

/// `~`: matches anywhere in the value.
fn contains_glob(value: &Text) -> Pat {
    if has_wildcard(value) {
        glob(&value.s, true, true)
    } else {
        Pat::Contains(value.s.to_lowercase())
    }
}

/// Build a wildcard pattern from `text`; `float` ends are unanchored.
fn glob(text: &str, float_start: bool, float_end: bool) -> Pat {
    let lower = text.to_lowercase();
    let lead = float_start || lower.starts_with('*');
    let trail = float_end || lower.ends_with('*');
    let parts: Vec<String> = lower
        .split('*')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    Pat::Glob { parts, lead, trail }
}

/// `N`, `N-M`, `N-` or `-M`.
fn parse_range(text: &str) -> Result<Range, ParseErrorKind> {
    let num = |s: &str| -> Result<u64, ParseErrorKind> {
        s.parse::<u64>()
            .map_err(|_| ParseErrorKind::InvalidNumber(s.to_string()))
    };
    match text.split_once('-') {
        None => {
            let n = num(text)?;
            Ok(Range {
                min: Some(n),
                max: Some(n),
            })
        }
        Some(("", "")) => Err(ParseErrorKind::InvalidNumber(text.to_string())),
        Some((lo, hi)) => {
            let min = if lo.is_empty() { None } else { Some(num(lo)?) };
            let max = if hi.is_empty() { None } else { Some(num(hi)?) };
            if let (Some(a), Some(b)) = (min, max) {
                if a > b {
                    return Err(ParseErrorKind::InvalidRange(text.to_string()));
                }
            }
            Ok(Range { min, max })
        }
    }
}
