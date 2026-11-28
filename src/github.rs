//! Bounded retrieval of remote manifests and repository availability.

use std::process::ExitStatus;
use std::time::{Duration, Instant};

const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROBE_BYTES: usize = 64;
const DOWNLOAD_BUDGET: Duration = Duration::from_secs(15);

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
}
