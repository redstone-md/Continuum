//! Literal / regex text search across the whole workspace -- the line-precise
//! complement to the symbol-scoped `search_code`.

use std::path::Path;

use continuum_core::dto::TextMatch;
use regex::RegexBuilder;

use crate::{is_skipped_dir, rel_path};

/// Longest matched line returned; longer lines are truncated.
const MAX_LINE_LEN: usize = 240;

/// Search every text file under `root` for `pattern`.
///
/// With `regex` false the pattern is matched literally; with it true the
/// pattern is a regular expression. The scan stops once `limit` matches are
/// collected. Binary and non-UTF-8 files are skipped, as are files above
/// `max_file_bytes` and the usual build/VCS directories.
pub fn search_text(
    root: &Path,
    pattern: &str,
    limit: usize,
    regex: bool,
    ignore_case: bool,
    max_file_bytes: u64,
) -> Result<Vec<TextMatch>, String> {
    if pattern.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let expr = if regex {
        pattern.to_string()
    } else {
        regex::escape(pattern)
    };
    let re = RegexBuilder::new(&expr)
        .case_insensitive(ignore_case)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| format!("invalid search pattern: {e}"))?;

    let mut matches = Vec::new();
    let walk = ignore::WalkBuilder::new(root)
        .require_git(false)
        .filter_entry(|entry| !is_skipped_dir(entry))
        .build()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_some_and(|t| t.is_file()));

    for entry in walk {
        let path = entry.path();
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() <= max_file_bytes => {}
            _ => continue,
        }
        // `read_to_string` fails on binary / non-UTF-8 files, which it skips.
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let rel = rel_path(root, path);
        for (number, line) in content.lines().enumerate() {
            if re.is_match(line) {
                matches.push(TextMatch {
                    path: rel.clone(),
                    line: number + 1,
                    text: truncate(line.trim(), MAX_LINE_LEN),
                });
                if matches.len() >= limit {
                    return Ok(matches);
                }
            }
        }
    }
    Ok(matches)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default per-file cap from `Settings::default()`.
    const CAP: u64 = 2 * 1024 * 1024;

    fn workspace(files: &[(&str, &str)]) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("continuum-textsearch-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, body) in files {
            std::fs::write(dir.join(name), body).unwrap();
        }
        dir
    }

    #[test]
    fn finds_a_literal_string_with_location() {
        let ws = workspace(&[("notes.txt", "alpha\nthe needle is here\nbeta\n")]);
        let hits = search_text(&ws, "needle", 50, false, false, CAP).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "notes.txt");
        assert_eq!(hits[0].line, 2);
        assert!(hits[0].text.contains("needle"));
    }

    #[test]
    fn literal_mode_does_not_treat_metacharacters_as_regex() {
        let ws = workspace(&[("a.txt", "call foo(bar) now\n")]);
        assert_eq!(
            search_text(&ws, "foo(bar)", 50, false, false, CAP)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn regex_mode_matches_patterns() {
        let ws = workspace(&[("a.txt", "v1.2.3\nnope\nv9.9.9\n")]);
        let hits = search_text(&ws, r"v\d+\.\d+\.\d+", 50, true, false, CAP).unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn ignore_case_matches_regardless_of_case() {
        let ws = workspace(&[("a.txt", "ERROR: boom\n")]);
        assert_eq!(
            search_text(&ws, "error", 50, false, true, CAP)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            search_text(&ws, "error", 50, false, false, CAP)
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn limit_caps_the_result_count() {
        let ws = workspace(&[("a.txt", "x\nx\nx\nx\nx\n")]);
        assert_eq!(
            search_text(&ws, "x", 3, false, false, CAP).unwrap().len(),
            3
        );
    }

    #[test]
    fn invalid_regex_is_an_error() {
        let ws = workspace(&[("a.txt", "anything\n")]);
        assert!(search_text(&ws, "(unclosed", 50, true, false, CAP).is_err());
    }

    #[test]
    fn respects_gitignore() {
        let ws = workspace(&[
            (".gitignore", "ignored.txt\n"),
            ("ignored.txt", "secret needle\n"),
            ("kept.txt", "visible needle\n"),
        ]);
        let hits = search_text(&ws, "needle", 50, false, false, CAP).unwrap();
        assert_eq!(hits.len(), 1, "the gitignored file must be skipped");
        assert_eq!(hits[0].path, "kept.txt");
    }
}
