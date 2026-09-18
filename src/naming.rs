//! File names for captures: a prefix the person chose, then a date pattern.
//!
//! The pattern is written the way people write dates, `YYYY-MM-DD HH-MM-SS`,
//! and not in strftime's `%Y-%m-%d`. That leaves one ambiguity, `MM`, which
//! is read as it is read aloud: the month, unless it follows an hour, when it
//! is the minutes. `HH-MM` is ten past two, not October.

use std::path::{Path, PathBuf};

use chrono::{Datelike, NaiveDateTime, Timelike};

/// The patterns the settings offer, first the default.
pub const PATTERNS: [&str; 4] = [
    "YYYY-MM-DD HH-MM-SS",
    "YYYY-MM-DD HH-MM",
    "YYYYMMDD-HHMMSS",
    "YYYY-MM-DD",
];

/// Expand `pattern` for `when`. Anything that is not a token is copied as it
/// stands.
pub fn expand(pattern: &str, when: NaiveDateTime) -> String {
    let mut out = String::new();
    let mut rest = pattern;
    let mut seen_hour = false;
    while !rest.is_empty() {
        let token = ["YYYY", "YY", "MM", "DD", "HH", "SS"]
            .into_iter()
            .find(|t| rest.starts_with(t));
        match token {
            Some(t) => {
                let text = match t {
                    "YYYY" => format!("{:04}", when.year()),
                    "YY" => format!("{:02}", when.year().rem_euclid(100)),
                    "MM" if seen_hour => format!("{:02}", when.minute()),
                    "MM" => format!("{:02}", when.month()),
                    "DD" => format!("{:02}", when.day()),
                    "HH" => {
                        seen_hour = true;
                        format!("{:02}", when.hour())
                    }
                    _ => format!("{:02}", when.second()),
                };
                out.push_str(&text);
                rest = &rest[t.len()..];
            }
            None => {
                let c = rest.chars().next().expect("not empty");
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
}

/// A file name that is safe on every filesystem a person might save to: no
/// path separators, no control characters, none of the characters FAT and
/// NTFS refuse, and not empty.
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        "Capture".into()
    } else {
        trimmed.into()
    }
}

/// `prefix pattern`, expanded and sanitised: the stem of a new capture.
pub fn stem(prefix: &str, pattern: &str, when: NaiveDateTime) -> String {
    let date = expand(pattern, when);
    let prefix = prefix.trim();
    sanitize(&if prefix.is_empty() {
        date
    } else if date.is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix} {date}")
    })
}

/// `dir/stem.ext`, or `dir/stem (2).ext` and so on if that is taken. Two
/// captures in the same second must not overwrite each other.
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let candidate = dir.join(format!("{stem}.{ext}"));
    if !candidate.exists() {
        return candidate;
    }
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}).{ext}")))
        .find(|p| !p.exists())
        .expect("an unbounded range finds a free name")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, s)
            .unwrap()
    }

    #[test]
    fn mm_after_an_hour_is_minutes() {
        let when = at(2026, 9, 15, 14, 10, 7);
        assert_eq!(expand("YYYY-MM-DD HH-MM", when), "2026-09-15 14-10");
        assert_eq!(expand("YYYY-MM-DD HH-MM-SS", when), "2026-09-15 14-10-07");
        assert_eq!(expand("YYYYMMDD-HHMMSS", when), "20260915-141007");
        assert_eq!(expand("DD.MM.YY", when), "15.09.26");
    }

    #[test]
    fn text_that_is_not_a_token_is_kept() {
        let when = at(2026, 1, 2, 3, 4, 5);
        assert_eq!(expand("take YYYY", when), "take 2026");
        assert_eq!(expand("", when), "");
    }

    #[test]
    fn names_are_made_safe() {
        assert_eq!(sanitize("a/b:c"), "a-b-c");
        assert_eq!(sanitize("  ..  "), "Capture");
        assert_eq!(sanitize("..hidden"), "hidden");
    }

    #[test]
    fn the_stem_joins_prefix_and_date() {
        let when = at(2026, 9, 15, 10, 24, 1);
        assert_eq!(
            stem("Raven Camera", "YYYY-MM-DD HH-MM-SS", when),
            "Raven Camera 2026-09-15 10-24-01"
        );
        assert_eq!(stem("", "YYYY-MM-DD", when), "2026-09-15");
        assert_eq!(stem("Clip", "", when), "Clip");
    }

    #[test]
    fn taken_names_get_a_number() {
        let dir = std::env::temp_dir().join(format!("raven-camera-naming-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "x", "png");
        std::fs::write(&first, b"").unwrap();
        let second = unique_path(&dir, "x", "png");
        assert_eq!(second, dir.join("x (2).png"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
