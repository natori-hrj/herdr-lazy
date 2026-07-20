//! The herdr plugin marketplace, as a searchable local index.
//!
//! herdr publishes the marketplace as one static JSON document — the same one herdr.dev
//! renders. Having it locally turns "find a plugin" from *open a browser, copy a name, come
//! back to the terminal* into something the manage pane can do directly.
//!
//! **This URL is not a documented API.** It was found by reading what the marketplace page
//! fetches, and it can change or disappear without notice. Everything here therefore fails
//! soft: if the index cannot be fetched or parsed, browsing is unavailable and every other
//! part of herdr-lazy carries on working. It is a convenience layered on top, never a
//! dependency of the manager.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;

use crate::json;

const INDEX_URL: &str = "https://assets.herdr.dev/plugins/index.json";

/// How long a cached copy is considered good.
///
/// herdr regenerates the index roughly every 30 minutes. Re-fetching 130 KB on every keypress
/// would be rude to someone else's CDN for data that barely moves, so the cache is the normal
/// path and refreshing is explicit (`r` in the browser, or a stale cache).
const CACHE_SECONDS: u64 = 6 * 60 * 60;

/// One plugin as the marketplace describes it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Entry {
    pub(crate) full_name: String,
    pub(crate) description: String,
    pub(crate) stars: u64,
    pub(crate) language: String,
    pub(crate) topics: Vec<String>,
}

impl Entry {
    /// Does this entry match every whitespace-separated term?
    ///
    /// All terms must match (narrowing as you type), but each may match any of name,
    /// description or topics — searching "worktree fzf" should find a worktree plugin whose
    /// description mentions fzf, without the user thinking about which field holds what.
    pub(crate) fn matches(&self, query: &str) -> bool {
        let haystack = format!(
            "{} {} {}",
            self.full_name,
            self.description,
            self.topics.join(" ")
        )
        .to_lowercase();
        query
            .split_whitespace()
            .all(|term| haystack.contains(&term.to_lowercase()))
    }
}

fn cache_path() -> PathBuf {
    crate::config_dir().join("marketplace.json")
}

fn cache_age_seconds() -> Option<u64> {
    let meta = fs::metadata(cache_path()).ok()?;
    let modified = meta.modified().ok()?;
    modified.elapsed().ok().map(|d| d.as_secs())
}

/// Download the index. Shells out, because std has no TLS — the same reasoning that has the
/// install script use curl.
fn download() -> Result<String, String> {
    let attempt = |prog: &str, args: &[&str]| -> Option<String> {
        let out = Command::new(prog).args(args).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let body = String::from_utf8_lossy(&out.stdout).to_string();
        if body.trim().is_empty() {
            None
        } else {
            Some(body)
        }
    };

    attempt(
        "curl",
        &["-fsL", "--max-time", "20", "--retry", "1", INDEX_URL],
    )
    .or_else(|| attempt("wget", &["-q", "-O", "-", "--timeout=20", INDEX_URL]))
    .ok_or_else(|| {
        format!(
            "could not fetch {} (curl/wget unavailable or offline)",
            INDEX_URL
        )
    })
}

/// Parse the index document into entries.
///
/// Tolerant by design: a missing optional field yields an empty value rather than discarding
/// the entry, because the shape here belongs to someone else and may gain or lose fields.
pub(crate) fn parse_index(body: &str) -> Result<Vec<Entry>, String> {
    let v =
        json::parse(body.trim()).map_err(|e| format!("marketplace index is not JSON: {}", e))?;
    let plugins = v
        .get("plugins")
        .and_then(|p| p.as_array())
        .ok_or("marketplace index has no `plugins` array")?;

    let mut out = Vec::with_capacity(plugins.len());
    for p in plugins {
        let full_name = p.str_field("fullName").unwrap_or_default().to_string();
        if full_name.is_empty() {
            continue; // nothing we could install
        }
        out.push(Entry {
            full_name,
            description: p.str_field("description").unwrap_or_default().to_string(),
            stars: p
                .get("stars")
                .and_then(|s| match s {
                    json::Value::Num(n) if *n >= 0.0 => Some(*n as u64),
                    _ => None,
                })
                .unwrap_or(0),
            language: p.str_field("language").unwrap_or_default().to_string(),
            topics: p
                .get("topics")
                .and_then(|t| t.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
        });
    }
    // Most-starred first: the browser shows the head of the list before anything is typed,
    // and that is the most useful default ordering there.
    out.sort_by(|a, b| {
        b.stars
            .cmp(&a.stars)
            .then_with(|| a.full_name.cmp(&b.full_name))
    });
    Ok(out)
}

/// Entries from cache, downloading when the cache is missing, stale, or `force` is set.
///
/// Returns the entries and a human note about where they came from, so the UI can be honest
/// about showing data that may be hours old.
pub(crate) fn load(force: bool) -> Result<(Vec<Entry>, String), String> {
    let age = cache_age_seconds();
    let fresh_enough = !force && age.map(|a| a < CACHE_SECONDS).unwrap_or(false);

    if fresh_enough {
        if let Ok(body) = fs::read_to_string(cache_path()) {
            if let Ok(entries) = parse_index(&body) {
                return Ok((entries, describe_age(age)));
            }
        }
    }

    match download() {
        Ok(body) => {
            let entries = parse_index(&body)?;
            // Cache only what parsed, so a corrupt download cannot poison later runs.
            let _ = write_cache(&body);
            Ok((entries, "just refreshed".to_string()))
        }
        Err(e) => {
            // Offline with a stale cache is far better than offline with nothing.
            if let Ok(body) = fs::read_to_string(cache_path()) {
                if let Ok(entries) = parse_index(&body) {
                    return Ok((entries, format!("{} (offline: {})", describe_age(age), e)));
                }
            }
            Err(e)
        }
    }
}

fn write_cache(body: &str) -> io::Result<()> {
    let p = cache_path();
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(p, body)
}

fn describe_age(age: Option<u64>) -> String {
    match age {
        None => "cached".to_string(),
        Some(s) if s < 90 => "just refreshed".to_string(),
        Some(s) if s < 3600 => format!("cached {}m ago", s / 60),
        Some(s) => format!("cached {}h ago", s / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from the real document (herdr.dev, 2026-07-21).
    const INDEX: &str = r#"{"schemaVersion":1,"generatedAt":"2026-07-20T21:30:11.892Z",
      "source":{"provider":"github","totalCount":2},
      "plugins":[
        {"id":1,"fullName":"smarzban/herdr-file-viewer","owner":"smarzban","name":"herdr-file-viewer",
         "description":"A git-aware, read-only file viewer for herdr.","url":"https://github.com/smarzban/herdr-file-viewer",
         "stars":181,"forks":0,"openIssues":0,"language":"Rust",
         "topics":["herdr","herdr-plugin","tui"],"createdAt":"2026-06-18T00:00:00Z"},
        {"id":2,"fullName":"devashish2203/herdr-worktrunk","owner":"devashish2203","name":"herdr-worktrunk",
         "description":"Interactive fzf picker for Worktrunk","url":"https://github.com/devashish2203/herdr-worktrunk",
         "stars":41,"language":"Shell","topics":["herdr-plugin","git-worktree"]}
      ]}"#;

    #[test]
    fn parses_the_real_index_shape() {
        let e = parse_index(INDEX).expect("should parse");
        assert_eq!(e.len(), 2);
        // Sorted by stars, descending.
        assert_eq!(e[0].full_name, "smarzban/herdr-file-viewer");
        assert_eq!(e[0].stars, 181);
        assert_eq!(e[0].language, "Rust");
        assert!(e[0].topics.contains(&"tui".to_string()));
        assert_eq!(e[1].stars, 41);
    }

    #[test]
    fn rejects_a_document_without_plugins() {
        assert!(parse_index(r#"{"schemaVersion":1}"#).is_err());
        assert!(parse_index("not json at all").is_err());
    }

    /// Missing optional fields must not drop the entry — the schema is not ours to rely on.
    #[test]
    fn tolerates_missing_optional_fields() {
        let e = parse_index(r#"{"plugins":[{"fullName":"a/b"}]}"#).unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].description, "");
        assert_eq!(e[0].stars, 0);
        assert!(e[0].topics.is_empty());
    }

    #[test]
    fn an_entry_without_a_name_is_useless_and_dropped() {
        let e = parse_index(r#"{"plugins":[{"stars":9},{"fullName":"a/b"}]}"#).unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].full_name, "a/b");
    }

    #[test]
    fn search_narrows_across_name_description_and_topics() {
        let e = parse_index(INDEX).unwrap();
        let find = |q: &str| -> Vec<&str> {
            e.iter()
                .filter(|x| x.matches(q))
                .map(|x| x.full_name.as_str())
                .collect()
        };
        assert_eq!(find("viewer"), vec!["smarzban/herdr-file-viewer"]);
        assert_eq!(
            find("fzf"),
            vec!["devashish2203/herdr-worktrunk"],
            "matches description"
        );
        assert_eq!(
            find("git-worktree"),
            vec!["devashish2203/herdr-worktrunk"],
            "matches topic"
        );
        // Every term must match, so a second word narrows rather than widens.
        assert_eq!(find("herdr viewer"), vec!["smarzban/herdr-file-viewer"]);
        assert!(find("viewer worktrunk").is_empty());
        assert_eq!(find("").len(), 2, "an empty query shows everything");
    }

    #[test]
    fn search_is_case_insensitive() {
        let e = parse_index(INDEX).unwrap();
        assert!(e[0].matches("FILE-Viewer"));
    }
}
