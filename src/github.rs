//! Just enough GitHub API to answer "what changed since I installed this?".
//!
//! The `↑` marker says a repo has been pushed to; it cannot say whether that push is worth
//! pulling. This fetches the commit subjects between the installed commit and the current
//! default branch, so the details view can show them before `u` is pressed.
//!
//! Deliberately narrow and fail-soft, like the marketplace: unauthenticated, one request per
//! plugin and only when its details are opened, and any failure yields "could not check"
//! rather than blocking the pane. The unauthenticated rate limit is 60/hour, which is ample
//! when the trigger is a human opening one plugin's details, and hopeless for anything that
//! tried to check everything at once — so nothing does.

use std::process::Command;

/// What changed since install, for the details view.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Changes {
    /// Commit subjects newer than the installed commit, most recent first.
    Commits(Vec<String>),
    /// More than we asked for — the install is far enough behind that we stopped counting.
    ManyPlus(usize),
    /// Could not tell: offline, rate-limited, or the API shape was not what we expected.
    Unknown(String),
}

const PAGE: usize = 30;

/// Commit subjects on `owner/repo`'s default branch that are newer than `installed_commit`.
pub(crate) fn changes_since(owner_repo: &str, installed_commit: &str) -> Changes {
    if installed_commit.is_empty() {
        return Changes::Unknown("no installed commit to compare against".to_string());
    }
    let url = format!(
        "https://api.github.com/repos/{}/commits?per_page={}",
        owner_repo, PAGE
    );
    let body = match fetch(&url) {
        Some(b) => b,
        None => return Changes::Unknown("could not reach github".to_string()),
    };
    parse_commits(&body, installed_commit)
}

/// Shell out to curl/wget — std has no TLS, the same reason the install script and the
/// marketplace fetch do.
fn fetch(url: &str) -> Option<String> {
    let try_one = |prog: &str, args: &[&str]| -> Option<String> {
        let out = Command::new(prog).args(args).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let body = String::from_utf8_lossy(&out.stdout).to_string();
        (!body.trim().is_empty()).then_some(body)
    };
    // A User-Agent is required by the GitHub API, or it answers 403.
    try_one(
        "curl",
        &[
            "-fsL",
            "--max-time",
            "15",
            "-H",
            "User-Agent: herdr-lazy",
            "-H",
            "Accept: application/vnd.github+json",
            url,
        ],
    )
    .or_else(|| {
        try_one(
            "wget",
            &[
                "-q",
                "-O",
                "-",
                "--timeout=15",
                "--header=User-Agent: herdr-lazy",
                url,
            ],
        )
    })
}

/// Walk the commit list until the installed commit appears; everything before it is new.
///
/// Kept separate from the fetch so it can be tested against captured payloads — the walk is
/// where the off-by-one lives (is the installed commit itself "new"? no), and that is exactly
/// what a test should pin rather than a live API.
pub(crate) fn parse_commits(body: &str, installed_commit: &str) -> Changes {
    let v = match crate::json::parse(body.trim()) {
        Ok(v) => v,
        Err(e) => return Changes::Unknown(format!("unexpected response: {}", e)),
    };
    let Some(arr) = v.as_array() else {
        // The API returns an object (not an array) for errors like rate limiting.
        let msg = v
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unexpected response shape");
        return Changes::Unknown(msg.to_string());
    };

    let mut subjects = Vec::new();
    for c in arr {
        let sha = c.str_field("sha").unwrap_or_default();
        // Match on a prefix: the installed commit may be recorded abbreviated.
        if !installed_commit.is_empty()
            && (sha == installed_commit || sha.starts_with(installed_commit))
        {
            return Changes::Commits(subjects);
        }
        let subject = c
            .get("commit")
            .and_then(|c| c.str_field("message"))
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        subjects.push(subject);
    }

    // Ran off the end without finding it: the install is more than a page behind, or the
    // branch was force-pushed and the commit is gone. Either way, "many" is the honest answer.
    Changes::ManyPlus(subjects.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(shas_and_subjects: &[(&str, &str)]) -> String {
        let items: Vec<String> = shas_and_subjects
            .iter()
            .map(|(sha, subj)| {
                format!(
                    r#"{{"sha":"{}","commit":{{"message":"{}"}}}}"#,
                    sha,
                    subj.replace('"', "'")
                )
            })
            .collect();
        format!("[{}]", items.join(","))
    }

    #[test]
    fn lists_commits_newer_than_the_installed_one() {
        let body = payload(&[
            ("ccc", "newest"),
            ("bbb", "middle"),
            ("aaa", "the installed one"),
            ("999", "older, should not appear"),
        ]);
        assert_eq!(
            parse_commits(&body, "aaa"),
            Changes::Commits(vec!["newest".into(), "middle".into()])
        );
    }

    #[test]
    fn the_installed_commit_itself_is_not_a_change() {
        let body = payload(&[("aaa", "the installed one")]);
        assert_eq!(parse_commits(&body, "aaa"), Changes::Commits(vec![]));
    }

    #[test]
    fn an_abbreviated_installed_commit_still_matches() {
        let body = payload(&[("ccc", "new"), ("abcdef123456", "installed")]);
        assert_eq!(
            parse_commits(&body, "abcdef1"),
            Changes::Commits(vec!["new".into()])
        );
    }

    /// The install is further back than one page, so the count is a floor, not a total.
    #[test]
    fn a_missing_installed_commit_is_reported_as_many() {
        let body = payload(&[("ccc", "a"), ("bbb", "b"), ("aaa", "c")]);
        assert_eq!(
            parse_commits(&body, "not-in-this-page"),
            Changes::ManyPlus(3)
        );
    }

    /// Rate-limit and other errors come back as a JSON object, not an array.
    #[test]
    fn an_api_error_object_is_reported_not_parsed_as_commits() {
        let body = r#"{"message":"API rate limit exceeded","documentation_url":"..."}"#;
        match parse_commits(body, "aaa") {
            Changes::Unknown(m) => assert!(m.contains("rate limit")),
            other => panic!("expected Unknown, got {:?}", other),
        }
    }

    #[test]
    fn garbage_is_unknown_not_a_panic() {
        assert!(matches!(
            parse_commits("not json", "aaa"),
            Changes::Unknown(_)
        ));
        assert!(matches!(
            changes_since("owner/repo", ""),
            Changes::Unknown(_)
        ));
    }

    /// Only the first line of a commit message is a subject; the body must not leak in.
    #[test]
    fn only_the_subject_line_is_kept() {
        let body = r#"[{"sha":"ccc","commit":{"message":"subject\n\nlong body here"}},{"sha":"aaa","commit":{"message":"installed"}}]"#;
        assert_eq!(
            parse_commits(body, "aaa"),
            Changes::Commits(vec!["subject".into()])
        );
    }
}
