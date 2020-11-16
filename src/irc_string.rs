
use std::fmt;

use regex::Regex;
use lazy_static::lazy_static;
use serde::Serialize;
use itertools::join;

#[derive(Debug, Clone, PartialEq, Hash, Serialize)]
/// An IRC-safe string with stripped control codes, trimmed whitespace, and a reasonable length
pub struct IrcString(String);

impl<S> From<S> for IrcString
where
    S: std::convert::AsRef<str>,
{
    fn from(s: S) -> Self {
        Self(sanitize(s.as_ref(), 450))
    }
}

impl IrcString {
    pub fn trunc<'a>(&'a self, max: usize) -> MaybeTruncated<'a> {
        truncate(&self.0, max)
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl std::ops::Deref for IrcString {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

pub fn sanitize(text: &str, max_bytes: usize) -> String {
    lazy_static! {
        // Collapse any whitespace to a single space
        static ref WHITESPACE: Regex = Regex::new(r"\s+").unwrap();

        // Strip control codes and multiple combining chars
        static ref CONTROL: Regex = Regex::new(r"\pC|(?:\pM{2})\pM+").unwrap();
    }

    let text = text.trim();
    let text = WHITESPACE.replace_all(&text, " ");
    let text = CONTROL.replace_all(&text, "");

    truncate(&text, max_bytes).to_string()
}

fn truncate<'a>(s: &'a str, max_bytes: usize) -> MaybeTruncated<'a> {
    use unicode_segmentation::UnicodeSegmentation;
    s
        .grapheme_indices(true)
        .map(|(i, c)| i + c.len())
        .take_while(|i| *i <= max_bytes)
        .last()
        .map(|i|
            if i < s.len() {
                MaybeTruncated::Yup(&s[..i])
            } else {
                MaybeTruncated::Nope(&s[..i])
            }
        )
        .unwrap_or(MaybeTruncated::Nope(&s[..0]))
}

pub enum MaybeTruncated<'a> {
    Yup(&'a str),
    Nope(&'a str),
}

impl fmt::Display for MaybeTruncated<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MaybeTruncated::Yup(s) => write!(f, "{}…", s),
            MaybeTruncated::Nope(s) => write!(f, "{}", s)
        }
    }
}
