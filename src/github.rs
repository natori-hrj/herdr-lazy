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

use std::process::ExitStatus;
use std::time::{Duration, Instant};

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
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROBE_BYTES: usize = 64;
const DOWNLOAD_BUDGET: Duration = Duration::from_secs(15);

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

/// Fetch a file from a repository at a ref, as raw text.
///
/// `HEAD` is the default rather than `main`: raw.githubusercontent resolves it to whatever the
/// repository calls its default branch, so nothing here has to guess a branch name.
/// Overridable so the fetch itself can be exercised, rather than only ever being proven by its
/// failure path — the same reason `scripts/fetch-or-build.sh` takes a base URL.
pub(crate) fn raw_file(owner_repo: &str, reference: Option<&str>, path: &str) -> Option<String> {
    let base = std::env::var("HERDR_LAZY_RAW_BASE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://raw.githubusercontent.com".to_string());
    fetch(&format!(
        "{}/{}/{}/{}",
        base,
        owner_repo,
        reference.unwrap_or("HEAD"),
        path
    ))
}

/// Whether a repository still answers.
///
/// The plain repository URL, not the API: unauthenticated API calls are capped at 60 an hour,
/// which a list of any size would eat, and only the status code is wanted here. A renamed or
/// transferred repository redirects and so resolves — which is right, since `plugin install`
/// follows the same redirect.
///
/// `None` means the question could not be asked (no network, no curl or wget), which is not the
/// same as an answer and must never be reported as one.
pub(crate) fn repo_exists(owner_repo: &str) -> Option<bool> {
    let base = std::env::var("HERDR_LAZY_REPO_BASE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://github.com".to_string());
    let url = format!("{}/{}", base, owner_repo);

    repo_exists_with(&url, crate::download::capture)
}

fn repo_exists_with(
    url: &str,
    mut capture: impl FnMut(&str, &[&str], usize, Instant) -> Option<(ExitStatus, Vec<u8>)>,
) -> Option<bool> {
    let deadline = Instant::now() + DOWNLOAD_BUDGET;
    let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };
    // Capture only the status code. A 404 is an answer, while a dead network is not.
    if let Some((status, bytes)) = capture(
        "curl",
        &[
            "-sIL",
            "--max-time",
            "15",
            "-o",
            null_device,
            "-w",
            "%{http_code}",
            "-H",
            "User-Agent: herdr-lazy",
            url,
        ],
        MAX_PROBE_BYTES,
        deadline,
    ) {
        if status.success() {
            let code = String::from_utf8_lossy(&bytes);
            if let Ok(n) = code.trim().parse::<u16>() {
                if n > 0 {
                    return Some(n < 400);
                }
            }
        }
    }
    // wget exits 8 on a server error response, which is an answer of "not found".
    let (status, _) = capture(
        "wget",
        &["-q", "--spider", "--timeout=15", url],
        MAX_PROBE_BYTES,
        deadline,
    )?;
    if status.success() {
        Some(true)
    } else if status.code() == Some(8) {
        Some(false)
    } else {
        None
    }
}

/// Shell out to curl/wget — std has no TLS, the same reason the install script and the
/// marketplace fetch do.
fn fetch(url: &str) -> Option<String> {
    let deadline = Instant::now() + DOWNLOAD_BUDGET;
    let try_one = |prog: &str, args: &[&str]| -> Option<String> {
        let (status, bytes) = crate::download::capture(prog, args, MAX_RESPONSE_BYTES, deadline)?;
        if !status.success() {
            return None;
        }
        let body = String::from_utf8_lossy(&bytes).to_string();
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

    fn exit_status(code: i32) -> ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            ExitStatus::from_raw(code << 8)
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            ExitStatus::from_raw(code as u32)
        }
    }

    #[test]
    fn valid_curl_status_is_an_answer_without_a_fallback() {
        for (code, exists) in [("200", true), ("301", true), ("404", false)] {
            let result = repo_exists_with(
                "https://example.test/owner/repo",
                |program, args, cap, _| {
                    assert_eq!(program, "curl");
                    assert!(args.contains(&"%{http_code}"));
                    assert_eq!(cap, MAX_PROBE_BYTES);
                    Some((exit_status(0), format!("{code}\n").into_bytes()))
                },
            );
            assert_eq!(result, Some(exists));
        }
    }

    #[test]
    fn wget_status_and_unavailable_network_share_the_curl_deadline() {
        for (curl, wget, expected) in [
            (Some((0, "not a status")), Some(8), Some(false)),
            (Some((0, "000")), Some(0), Some(true)),
            (Some((6, "200")), Some(4), None),
            (None, None, None),
        ] {
            let mut attempted = Vec::new();
            let result = repo_exists_with(
                "https://example.test/owner/repo",
                |program, args, cap, deadline| {
                    assert_eq!(cap, MAX_PROBE_BYTES);
                    attempted.push((program.to_string(), deadline));
                    if program == "curl" {
                        curl.map(|(code, text)| (exit_status(code), text.as_bytes().to_vec()))
                    } else {
                        assert_eq!(program, "wget");
                        assert!(args.contains(&"--spider"));
                        wget.map(|code| (exit_status(code), Vec::new()))
                    }
                },
            );
            assert_eq!(result, expected);
            assert_eq!(attempted.len(), 2);
            assert_eq!(attempted[0].0, "curl");
            assert_eq!(attempted[1].0, "wget");
            assert_eq!(attempted[0].1, attempted[1].1);
        }
    }

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
