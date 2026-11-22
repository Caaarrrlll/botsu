//! Log output. `BOTSU_LOG` picks the levels to print, in any combination: `ERROR`, `WARN`,
//! `INFO`, `DEBUG`, `TRACE`, or `ALL` (comma or space separated, any case). Only those levels
//! are printed: `DEBUG,ERROR` shows debug and errors and nothing in between. Blank means
//! `ERROR,WARN,INFO`.
//!
//! The levels apply to botsu's own logs. Libraries (serenity, songbird, LiveKit, ...) only ever
//! print their warnings and errors, and only when those levels are picked: their debug output
//! would flood the log. `RUST_LOG` (tracing's EnvFilter syntax) overrides all of this for fine
//! control, for example `RUST_LOG=info,songbird=debug`.
//!
//! Both are read from the environment first, then from `<data dir>/botsu.env`, like every
//! other setting. Logging starts before the rest of the config, so they're looked up here.

use tracing::Level;
use tracing_subscriber::filter::{filter_fn, EnvFilter};
use tracing_subscriber::prelude::*;

use crate::config;

/// The levels `BOTSU_LOG` turned on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Levels {
    error: bool,
    warn: bool,
    info: bool,
    debug: bool,
    trace: bool,
}

impl Levels {
    const DEFAULT: Levels = Levels {
        error: true,
        warn: true,
        info: true,
        debug: false,
        trace: false,
    };

    const NONE: Levels = Levels {
        error: false,
        warn: false,
        info: false,
        debug: false,
        trace: false,
    };

    /// Parses a `BOTSU_LOG` value. Returns the levels and the words it didn't understand.
    pub fn parse(value: &str) -> (Levels, Vec<String>) {
        let words: Vec<&str> = value
            .split([',', ' ', ';', '|'])
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .collect();
        if words.is_empty() {
            return (Levels::DEFAULT, Vec::new());
        }

        let mut levels = Levels::NONE;
        let mut unknown = Vec::new();
        for word in words {
            match word.to_ascii_uppercase().as_str() {
                "ALL" => {
                    levels = Levels {
                        error: true,
                        warn: true,
                        info: true,
                        debug: true,
                        trace: true,
                    };
                }
                "ERROR" => levels.error = true,
                "WARN" | "WARNING" => levels.warn = true,
                "INFO" => levels.info = true,
                "DEBUG" => levels.debug = true,
                "TRACE" => levels.trace = true,
                _ => unknown.push(word.to_owned()),
            }
        }

        // Nothing usable at all: keep the default rather than printing nothing.
        if levels == Levels::NONE {
            levels = Levels::DEFAULT;
        }
        (levels, unknown)
    }

    fn has(&self, level: &Level) -> bool {
        match *level {
            Level::ERROR => self.error,
            Level::WARN => self.warn,
            Level::INFO => self.info,
            Level::DEBUG => self.debug,
            Level::TRACE => self.trace,
        }
    }

    /// Whether a log line from `target` at `level` is printed.
    pub fn allows(&self, target: &str, level: &Level) -> bool {
        let ours =
            target == "botsu" || target.starts_with("botsu_") || target.starts_with("botsu::");
        let library_level = matches!(*level, Level::ERROR | Level::WARN);
        self.has(level) && (ours || library_level)
    }
}

/// Which setting decides the log output.
#[derive(Debug, PartialEq, Eq)]
enum Source {
    /// `RUST_LOG`, tracing's own filter syntax.
    RustLog(String),
    /// `BOTSU_LOG` (possibly blank: the default levels).
    BotsuLog(String),
}

/// The environment beats the settings file, whichever variable is used: a `BOTSU_LOG` in the
/// Unraid template wins over an old `RUST_LOG` line in botsu.env. Within one place, `RUST_LOG`
/// wins. Blank values count as unset.
fn choose(env: impl Fn(&str) -> Option<String>, file: impl Fn(&str) -> Option<String>) -> Source {
    let clean = |v: Option<String>| v.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    for get in [&env as &dyn Fn(&str) -> Option<String>, &file] {
        if let Some(v) = clean(get("RUST_LOG")) {
            return Source::RustLog(v);
        }
        if let Some(v) = clean(get("BOTSU_LOG")) {
            return Source::BotsuLog(v);
        }
    }
    Source::BotsuLog(String::new())
}

/// Starts logging to stdout (Docker keeps it; see `docker logs`).
pub fn init() {
    // The settings file may not exist yet, or may be unreadable: logging still starts.
    let path = config::data_dir().join(config::SETTINGS_FILE);
    let file = config::read_settings(&path).unwrap_or_default();
    let source = choose(|k| std::env::var(k).ok(), |k| file.get(k).cloned());

    let botsu_log = match source {
        Source::RustLog(rust_log) => {
            match EnvFilter::try_new(&rust_log) {
                Ok(filter) => {
                    tracing_subscriber::registry()
                        .with(tracing_subscriber::fmt::layer().with_filter(filter))
                        .init();
                    tracing::info!(RUST_LOG = %rust_log, "log filter from RUST_LOG");
                    return;
                }
                Err(e) => {
                    eprintln!("RUST_LOG={rust_log:?} isn't a valid filter ({e}); using the default levels");
                    String::new()
                }
            }
        }
        Source::BotsuLog(value) => value,
    };

    let (levels, unknown) = Levels::parse(&botsu_log);
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer().with_filter(filter_fn(move |meta| {
                levels.allows(meta.target(), meta.level())
            })),
        )
        .init();
    if !unknown.is_empty() {
        tracing::warn!(
            ignored = ?unknown,
            "BOTSU_LOG has words it doesn't know; use ERROR, WARN, INFO, DEBUG, TRACE or ALL"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn environment_beats_the_file() {
        // Template BOTSU_LOG vs an old RUST_LOG line in botsu.env.
        let src = choose(
            vars(&[("BOTSU_LOG", "DEBUG")]),
            vars(&[("RUST_LOG", "info,botsu=debug")]),
        );
        assert_eq!(src, Source::BotsuLog("DEBUG".into()));
        // Same place: RUST_LOG wins.
        let src = choose(
            vars(&[("BOTSU_LOG", "DEBUG"), ("RUST_LOG", "warn")]),
            vars(&[]),
        );
        assert_eq!(src, Source::RustLog("warn".into()));
        // Blank template fields don't count; the file decides.
        let src = choose(
            vars(&[("RUST_LOG", " "), ("BOTSU_LOG", "")]),
            vars(&[("BOTSU_LOG", "ALL")]),
        );
        assert_eq!(src, Source::BotsuLog("ALL".into()));
        assert_eq!(
            choose(vars(&[]), vars(&[])),
            Source::BotsuLog(String::new())
        );
    }

    #[test]
    fn any_combination() {
        let (l, unknown) = Levels::parse("debug, ERROR");
        assert!(unknown.is_empty());
        assert!(l.allows("botsu_core::alerts", &Level::DEBUG));
        assert!(l.allows("botsu", &Level::ERROR));
        // Only the listed levels: nothing in between.
        assert!(!l.allows("botsu", &Level::INFO));
        assert!(!l.allows("botsu", &Level::WARN));
    }

    #[test]
    fn all_and_default() {
        let (all, _) = Levels::parse("ALL");
        assert!(all.allows("botsu_discord", &Level::TRACE));
        let (blank, _) = Levels::parse("  ");
        assert_eq!(blank, Levels::DEFAULT);
        assert!(blank.allows("botsu", &Level::INFO));
        assert!(!blank.allows("botsu", &Level::DEBUG));
    }

    #[test]
    fn libraries_only_warn_and_error() {
        let (all, _) = Levels::parse("ALL");
        assert!(!all.allows("serenity::gateway", &Level::DEBUG));
        assert!(!all.allows("livekit", &Level::INFO));
        assert!(all.allows("songbird::driver", &Level::WARN));
        let (info, _) = Levels::parse("INFO");
        assert!(
            !info.allows("songbird::driver", &Level::WARN),
            "WARN wasn't picked"
        );
    }

    #[test]
    fn unknown_words_are_reported() {
        let (l, unknown) = Levels::parse("INFO, loud");
        assert_eq!(unknown, vec!["loud".to_owned()]);
        assert!(l.allows("botsu", &Level::INFO));
        let (nothing, _) = Levels::parse("loud");
        assert_eq!(nothing, Levels::DEFAULT, "no usable word: default");
    }
}
