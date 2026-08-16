//! Runtime configuration, read from `CONTINUUM_*` environment variables.
//!
//! This is the only module that reads the `CONTINUUM_*` tuning variables;
//! every other crate receives its settings as plain values. That keeps the
//! configuration injectable — tests construct a `Settings` directly instead of
//! mutating process-global state. The variable catalogue lives in README.md.
//!
//! The one variable that stays outside: `CONTINUUM_IDLE_MINUTES`, which the
//! daemon exposes as the `--idle-minutes` CLI flag and clap reads directly.

use std::time::Duration;

/// Default HuggingFace repo of the static embedding model (~30 MB).
pub const DEFAULT_MODEL_REPO: &str = "minishlab/potion-base-8M";

/// Largest file indexed, by default.
const DEFAULT_MAX_FILE_KIB: u64 = 2 * 1024;

/// Default per-pass ceiling on files pulled into the index.
const DEFAULT_MAX_FILES: usize = 50_000;

/// Default watcher debounce window.
const DEFAULT_DEBOUNCE_MS: u64 = 300;

/// Runtime configuration. See README.md for the variable catalogue.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Allow indexing a workspace rooted at a drive/filesystem root or the
    /// user's home directory (`CONTINUUM_ALLOW_LARGE_ROOT`).
    pub allow_large_root: bool,
    /// Load the embedding model at startup instead of lazily on first
    /// `search_code` (`CONTINUUM_PRELOAD_MODEL`).
    pub preload_model: bool,
    /// Ceiling on files pulled into one index pass; `usize::MAX` disables the
    /// cap (`CONTINUUM_MAX_FILES`, `0` disables).
    pub max_files: usize,
    /// Largest file indexed, in bytes (`CONTINUUM_MAX_FILE_KIB`).
    pub max_file_bytes: u64,
    /// Watcher debounce window (`CONTINUUM_DEBOUNCE_MS`).
    pub debounce: Duration,
    /// HuggingFace repo of the embedding model, or `off`/`none` to disable
    /// semantic search (`CONTINUUM_MODEL`).
    pub model_repo: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            allow_large_root: false,
            preload_model: false,
            max_files: DEFAULT_MAX_FILES,
            max_file_bytes: DEFAULT_MAX_FILE_KIB * 1024,
            debounce: Duration::from_millis(DEFAULT_DEBOUNCE_MS),
            model_repo: DEFAULT_MODEL_REPO.to_string(),
        }
    }
}

impl Settings {
    /// Read settings from the process environment; invalid or missing values
    /// fall back to the defaults.
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Build settings from a lookup function — the seam tests use to supply
    /// values without touching the environment.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let mut settings = Self::default();

        if flag(&lookup, "CONTINUUM_ALLOW_LARGE_ROOT") {
            settings.allow_large_root = true;
        }
        if flag(&lookup, "CONTINUUM_PRELOAD_MODEL") {
            settings.preload_model = true;
        }
        if let Some(files) = parse_uint::<usize>(&lookup, "CONTINUUM_MAX_FILES") {
            settings.max_files = if files == 0 { usize::MAX } else { files };
        }
        if let Some(kib) = parse_uint::<u64>(&lookup, "CONTINUUM_MAX_FILE_KIB") {
            settings.max_file_bytes = kib.saturating_mul(1024);
        }
        if let Some(ms) = parse_uint::<u64>(&lookup, "CONTINUUM_DEBOUNCE_MS") {
            settings.debounce = Duration::from_millis(ms);
        }
        if let Some(repo) = lookup("CONTINUUM_MODEL") {
            settings.model_repo = repo;
        }

        settings
    }
}

/// Whether a variable is set to a truthy value.
fn flag(lookup: &impl Fn(&str) -> Option<String>, name: &str) -> bool {
    lookup(name).is_some_and(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
}

/// A parseable unsigned integer; `None` on missing or malformed input.
fn parse_uint<T: std::str::FromStr>(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &str,
) -> Option<T> {
    lookup(name)?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        }
    }

    #[test]
    fn defaults_when_nothing_is_set() {
        let s = Settings::from_lookup(lookup(&[]));
        assert!(!s.allow_large_root);
        assert_eq!(s.max_files, 50_000);
        assert_eq!(s.max_file_bytes, 2 * 1024 * 1024);
        assert_eq!(s.debounce, Duration::from_millis(300));
        assert_eq!(s.model_repo, DEFAULT_MODEL_REPO);
    }

    #[test]
    fn flags_accept_truthy_values() {
        let s = Settings::from_lookup(lookup(&[
            ("CONTINUUM_ALLOW_LARGE_ROOT", "1"),
            ("CONTINUUM_PRELOAD_MODEL", " yes\n"),
        ]));
        assert!(s.allow_large_root);
        assert!(s.preload_model);
    }

    #[test]
    fn non_truthy_flag_values_are_ignored() {
        let s = Settings::from_lookup(lookup(&[("CONTINUUM_PRELOAD_MODEL", "0")]));
        assert!(!s.preload_model);
    }

    #[test]
    fn max_files_zero_disables_the_cap() {
        let s = Settings::from_lookup(lookup(&[("CONTINUUM_MAX_FILES", "0")]));
        assert_eq!(s.max_files, usize::MAX);
    }

    #[test]
    fn max_files_parses_and_kib_converts_to_bytes() {
        let s = Settings::from_lookup(lookup(&[
            ("CONTINUUM_MAX_FILES", "1000"),
            ("CONTINUUM_MAX_FILE_KIB", "512"),
        ]));
        assert_eq!(s.max_files, 1000);
        assert_eq!(s.max_file_bytes, 512 * 1024);
    }

    #[test]
    fn invalid_values_fall_back_to_defaults() {
        let s = Settings::from_lookup(lookup(&[
            ("CONTINUUM_MAX_FILES", "many"),
            ("CONTINUUM_MAX_FILE_KIB", "-5"),
            ("CONTINUUM_DEBOUNCE_MS", "soon"),
        ]));
        assert_eq!(s.max_files, 50_000);
        assert_eq!(s.max_file_bytes, 2 * 1024 * 1024);
        assert_eq!(s.debounce, Duration::from_millis(300));
        assert_eq!(s.model_repo, DEFAULT_MODEL_REPO);
    }

    #[test]
    fn blank_model_override_is_passed_through_as_disabled() {
        // An explicitly blank CONTINUUM_MODEL disables semantic search (the
        // embedder bails on a blank repo), matching the pre-Settings behavior.
        let s = Settings::from_lookup(lookup(&[("CONTINUUM_MODEL", "  ")]));
        assert!(s.model_repo.trim().is_empty());
    }

    #[test]
    fn model_repo_override_passes_through() {
        let s = Settings::from_lookup(lookup(&[("CONTINUUM_MODEL", "org/other-model")]));
        assert_eq!(s.model_repo, "org/other-model");
    }
}
