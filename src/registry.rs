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
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Entry {
    pub(crate) full_name: String,
    pub(crate) description: String,
    pub(crate) stars: u64,
    pub(crate) language: String,
    pub(crate) topics: Vec<String>,
    /// The repository page. Kept so the browser can open it — reading a stranger's code
    /// before installing it is the habit worth supporting, and without this the browser is a
    /// dead end that can install but not evaluate.
    pub(crate) url: String,
    /// `pushedAt`, verbatim (`2026-07-17T06:37:31Z`). When a plugin was last touched says
    /// more about whether it still works than its star count does.
    pub(crate) pushed_at: String,
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

/// Days since the Unix epoch for a `YYYY-MM-DD…` string, or None if it is not one.
///
/// Hand-rolled rather than pulling in a date crate for one field. This is the standard
/// civil-date-to-days conversion; it is exact for any date the marketplace can contain.
fn days_from_iso(s: &str) -> Option<i64> {
    let (y, rest) = s.split_once('-')?;
    let (m, rest) = rest.split_once('-')?;
    let d = rest.get(..2)?;
    let (y, m, d): (i64, i64, i64) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + if m > 2 { -3 } else { 9 }) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// "3d", "2w", "5mo" — how long ago, in as few characters as possible.
pub(crate) fn age_label(pushed_at: &str, today: i64) -> String {
    let Some(then) = days_from_iso(pushed_at) else {
        return String::new();
    };
    let days = (today - then).max(0);
    // Boundaries chosen so no bucket can render a leading zero: at 28 days `days / 30` is 0,
    // which showed as "0mo" — a label that reads as "never" for something touched last month.
    match days {
        0 => "today".to_string(),
        1..=6 => format!("{}d", days),
        7..=29 => format!("{}w", days / 7),
        30..=364 => format!("{}mo", days / 30),
        _ => format!("{}y", days / 365),
    }
}

/// Has this repository been pushed to since the given instant?
///
/// The comparison behind the "update?" marker. Both halves are approximations and the result
/// is worded as a question on purpose:
///
///   - `pushedAt` is the last push to *any* branch, so a tag or a topic branch counts even
///     though the default branch — the thing herdr installs — has not moved.
///   - a plugin installed at a pinned commit is deliberately not tracking the branch, so it
///     is excluded by the caller rather than reported as stale.
///
/// A false "maybe" costs a wasted `u`; a false "you are current" would hide real updates,
/// which is the worse failure, so it errs toward reporting.
pub(crate) fn pushed_since(pushed_at: &str, installed_unix_ms: u64) -> bool {
    let Some(pushed_day) = days_from_iso(pushed_at) else {
        return false;
    };
    let installed_day = (installed_unix_ms / 1000 / 86_400) as i64;
    // Day resolution: the index only carries a date we can rely on parsing, and an install
    // and a push on the same day is not worth flagging either way.
    pushed_day > installed_day
}

/// Today, as days since the epoch.
pub(crate) fn today_days() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86_400) as i64)
        .unwrap_or(0)
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
            url: p.str_field("url").unwrap_or_default().to_string(),
            pushed_at: p.str_field("pushedAt").unwrap_or_default().to_string(),
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

/// Entries from the cache, or nothing. Never fetches.
///
/// Used by the list view, which runs on every keypress-driven redraw and must not depend on
/// the network being there — or on someone else's CDN being willing.
pub(crate) fn cached_entries() -> Vec<Entry> {
    fs::read_to_string(cache_path())
        .ok()
        .and_then(|b| parse_index(&b).ok())
        .unwrap_or_default()
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
         "topics":["herdr","herdr-plugin","tui"],"createdAt":"2026-06-18T00:00:00Z","pushedAt":"2026-07-17T06:37:31Z"},
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
    fn parses_url_and_last_push() {
        let e = parse_index(INDEX).unwrap();
        assert_eq!(e[0].url, "https://github.com/smarzban/herdr-file-viewer");
        assert_eq!(e[0].pushed_at, "2026-07-17T06:37:31Z");
    }

    #[test]
    fn converts_iso_dates_to_days() {
        assert_eq!(days_from_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(days_from_iso("1970-01-02T00:00:00Z"), Some(1));
        assert_eq!(days_from_iso("2000-03-01T00:00:00Z"), Some(11017));
        // Leap day must not shift the count.
        assert_eq!(
            days_from_iso("2024-03-01").unwrap() - days_from_iso("2024-02-28").unwrap(),
            2
        );
        assert_eq!(days_from_iso("not a date"), None);
        assert_eq!(days_from_iso("2026-13-01"), None, "month out of range");
    }

    #[test]
    fn a_push_after_the_install_is_reported() {
        let day = 86_400_000u64;
        // installed on day 20000, pushed on day 20001
        let installed = 20_000 * day;
        assert!(
            pushed_since("2024-10-05T00:00:00Z", 0),
            "any push beats epoch"
        );
        assert!(!pushed_since("", installed), "no date, no claim");
        assert!(!pushed_since("not a date", installed));
    }

    #[test]
    fn same_day_is_not_an_update() {
        // 2026-07-21 as ms; a push the same day must not be flagged.
        let d = days_from_iso("2026-07-21").unwrap() as u64;
        let installed_ms = d * 86_400_000;
        assert!(!pushed_since("2026-07-21T23:59:00Z", installed_ms));
        assert!(pushed_since("2026-07-22T00:01:00Z", installed_ms));
        assert!(!pushed_since("2026-07-20T00:01:00Z", installed_ms));
    }

    #[test]
    fn ages_read_as_few_characters_as_possible() {
        let today = days_from_iso("2026-07-21").unwrap();
        let ago = |d: i64| age_label(&format!("{}T00:00:00Z", iso_of(today - d)), today);
        assert_eq!(ago(0), "today");
        assert_eq!(ago(3), "3d");
        assert_eq!(ago(14), "2w");
        assert_eq!(ago(60), "2mo");
        assert_eq!(ago(400), "1y");
        // Every bucket boundary, since an off-by-one here renders "0mo" or "0y".
        for d in 0..800 {
            let label = ago(d);
            assert!(
                !label.starts_with('0') || label == "0d",
                "{} days rendered as {}",
                d,
                label
            );
        }
        assert_eq!(ago(29), "4w");
        assert_eq!(ago(30), "1mo");
        assert_eq!(ago(364), "12mo");
        assert_eq!(ago(365), "1y");
    }

    /// A missing or malformed date must render as nothing, not as a wrong age.
    #[test]
    fn an_unparseable_date_has_no_age() {
        assert_eq!(age_label("", 20_000), "");
        assert_eq!(age_label("soon", 20_000), "");
    }

    /// Days-since-epoch back to `YYYY-MM-DD`, for the test above only.
    fn iso_of(days: i64) -> String {
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        format!("{:04}-{:02}-{:02}", y, m, d)
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
