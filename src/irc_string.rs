use std::fmt;

use itertools::join;
use lazy_static::lazy_static;
use regex::Regex;
use serde::Serialize;

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
    pub fn trunc(&'_ self, max: usize) -> MaybeTruncated<'_> {
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

/// Collapse all whitespace, strip control codes and obvious combining character abuse,
/// And truncate to a given size, appending a unicode ellipsis if appropriate.
/// Will overshoot max_bytes by 3 because of that.
pub fn sanitize(text: &str, max_bytes: usize) -> String {
    lazy_static! {
        static ref CONTROL: Regex = Regex::new(r"\pC|(?:\pM{2})\pM+").unwrap();
    }

    let text = join(
        text.split_whitespace().map(|s| CONTROL.replace_all(s, "")),
        " ",
    );

    truncate(&text, max_bytes).to_string()
}

#[test]
fn vaguely_test_sanitize() {
    let tests = vec![
        (" foo  bar  baz ", "foo bar baz"),
        ("foo\nbar\tbaz", "foo bar baz"),
        ("Z̡̢̖͛̍ͫ̂̚͜A̸̶̡̩͖͉̟̞̺ͨ̎̓ͭ̇̂Ḻ̵͋́̃͝͡G̪̹͌̋ͅǪ̖̐ͭ̑!͚͙͈̐͢", "ZALGO!"),
        ("0123456789abcdefghijklm", "0123456789abcdef…"),
    ];

    for (src, tgt) in tests {
        assert_eq!(sanitize(src, 16), tgt);
    }
}

fn truncate(s: &'_ str, max_bytes: usize) -> MaybeTruncated<'_> {
    use unicode_segmentation::UnicodeSegmentation;
    s.grapheme_indices(true)
        .map(|(i, c)| i + c.len())
        .take_while(|i| *i <= max_bytes)
        .last()
        .map(|i| {
            if i < s.len() {
                MaybeTruncated::Yup(&s[..i])
            } else {
                MaybeTruncated::Nope(&s[..i])
            }
        })
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
            MaybeTruncated::Nope(s) => write!(f, "{}", s),
        }
    }
}

impl fmt::Display for IrcString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
