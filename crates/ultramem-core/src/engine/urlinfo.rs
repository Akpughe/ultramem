//! URL understanding for browser captures. A visit record that just says
//! "Visited web page <url>" is nearly invisible to semantic search; this
//! module turns known URL shapes into meaningful text — "GitHub pull request
//! #826 in 500chaw/chowcentral-api-backend" — so questions like "show me the
//! PR links from this week" actually match. Also classifies junk URLs that
//! pollute memory without ever answering a question.

/// Hosts/fragments that never carry memorable content.
const JUNK: [&str; 15] = [
    "instagram.com/auth_platform",
    "claudeusercontent.com",
    "googleusercontent.com",
    "accounts.google.com",
    "accounts.youtube.com",
    "doubleclick.net",
    "googleads.",
    "googlesyndication",
    "/oauth/authorize",
    "/oauth2/",
    "auth0.com/authorize",
    "okta.com/oauth",
    "/login/callback",
    "chrome-extension://",
    "/cdn-cgi/",
];

pub fn is_junk(url: &str) -> bool {
    if !url.starts_with("http") {
        return true;
    }
    let lower = url.to_lowercase();
    JUNK.iter().any(|j| lower.contains(j))
}

/// (host, path segments, query) without a URL-parsing dependency.
fn split(url: &str) -> (String, Vec<String>, String) {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let (host_path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut parts = host_path.split('/');
    let host = parts
        .next()
        .unwrap_or_default()
        .trim_start_matches("www.")
        .to_lowercase();
    let segs: Vec<String> = parts
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    (host, segs, query.to_string())
}

fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == name).then(|| {
            v.replace('+', " ")
                .split('%')
                .enumerate()
                .map(|(i, s)| {
                    if i == 0 {
                        s.to_string()
                    } else if s.len() >= 2 {
                        u8::from_str_radix(&s[..2], 16)
                            .map(|b| format!("{}{}", b as char, &s[2..]))
                            .unwrap_or_else(|_| format!("%{s}"))
                    } else {
                        format!("%{s}")
                    }
                })
                .collect::<String>()
        })
    })
}

/// What this URL *is*, in words. `page_title` is the browser's title for it.
/// Returns (display_title, description) — both end up embedded, so the words
/// here are what make link questions answerable.
pub fn describe(url: &str, page_title: &str) -> (String, String) {
    let (host, segs, query) = split(url);
    let title = page_title.trim();
    let titled = |fallback: String| {
        if title.is_empty() {
            fallback
        } else {
            title.to_string()
        }
    };
    let s = |i: usize| segs.get(i).map(String::as_str).unwrap_or("");

    let desc = match host.as_str() {
        "github.com" => {
            let repo = format!("{}/{}", s(0), s(1));
            match s(2) {
                "pull" => format!("GitHub pull request #{} in the {repo} repository", s(3)),
                "issues" if !s(3).is_empty() => {
                    format!("GitHub issue #{} in the {repo} repository", s(3))
                }
                "commit" => format!(
                    "GitHub commit {} in the {repo} repository",
                    &s(3)[..s(3).len().min(10)]
                ),
                "releases" => format!("GitHub releases for the {repo} repository"),
                "tree" | "blob" => format!("File or folder in the GitHub repository {repo}"),
                "" if segs.len() == 1 => format!("GitHub profile of {}", s(0)),
                _ if segs.len() >= 2 => format!("GitHub repository {repo}"),
                _ => "GitHub page".into(),
            }
        }
        "youtube.com" | "m.youtube.com" => {
            if s(0) == "watch" || s(0) == "shorts" {
                "YouTube video".into()
            } else if s(0) == "results" {
                format!(
                    "YouTube search for \"{}\"",
                    query_param(&query, "search_query").unwrap_or_default()
                )
            } else if let Some(handle) = segs.first().filter(|x| x.starts_with('@')) {
                format!("YouTube channel {handle}")
            } else {
                "YouTube page".into()
            }
        }
        "youtu.be" => "YouTube video".into(),
        "google.com" if s(0) == "search" => {
            format!(
                "Google search for \"{}\"",
                query_param(&query, "q").unwrap_or_default()
            )
        }
        "stackoverflow.com" if s(0) == "questions" => "Stack Overflow question".into(),
        "x.com" | "twitter.com" => {
            if s(1) == "status" {
                format!("Post on X by @{}", s(0))
            } else if !s(0).is_empty() {
                format!("X profile of @{}", s(0))
            } else {
                "X (Twitter) page".into()
            }
        }
        "reddit.com" if s(0) == "r" => format!("Reddit post in r/{}", s(1)),
        "linkedin.com" if s(0) == "in" => format!("LinkedIn profile of {}", s(1)),
        "mail.google.com" => "Gmail".into(),
        "docs.google.com" => format!(
            "Google {} document",
            match s(0) {
                "spreadsheets" => "Sheets",
                "presentation" => "Slides",
                _ => "Docs",
            }
        ),
        "chatgpt.com" | "chat.openai.com" => "ChatGPT conversation".into(),
        "claude.ai" => "Claude conversation".into(),
        _ => {
            let site = host.split('.').rev().nth(1).map(|s| {
                let mut c = s.chars();
                c.next()
                    .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                    .unwrap_or_default()
            });
            format!("Page on {}", site.unwrap_or_else(|| host.clone()))
        }
    };

    (titled(desc.clone()), desc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_pr_is_understood() {
        let (_, d) = describe(
            "https://github.com/500chaw/chowcentral-api-backend/pull/826",
            "fix(company-order): close gaps · Pull Request #826",
        );
        assert_eq!(
            d,
            "GitHub pull request #826 in the 500chaw/chowcentral-api-backend repository"
        );
    }

    #[test]
    fn github_repo_and_commit() {
        assert_eq!(
            describe("https://github.com/qdrant/qdrant", "").1,
            "GitHub repository qdrant/qdrant"
        );
        assert!(
            describe("https://github.com/a/b/commit/abc123def456789", "")
                .1
                .starts_with("GitHub commit abc123def4")
        );
    }

    #[test]
    fn youtube_and_search_understood() {
        assert_eq!(
            describe("https://www.youtube.com/watch?v=xyz", "Cool video").1,
            "YouTube video"
        );
        assert_eq!(
            describe("https://www.google.com/search?q=qdrant+hybrid%20search", "").1,
            "Google search for \"qdrant hybrid search\""
        );
    }

    #[test]
    fn empty_titles_fall_back_to_description() {
        let (t, _) = describe("https://github.com/davak/recally/pull/12", "");
        assert_eq!(t, "GitHub pull request #12 in the davak/recally repository");
    }

    #[test]
    fn junk_urls_are_rejected() {
        assert!(is_junk("https://019ddaf1-f1c4.claudeusercontent.com/v1/x"));
        assert!(is_junk("https://accounts.google.com/signin"));
        assert!(is_junk("chrome://settings"));
        assert!(!is_junk("https://github.com/davak/recally"));
    }

    #[test]
    fn generic_domains_become_readable() {
        assert_eq!(
            describe("https://news.ycombinator.com/item?id=1", "").1,
            "Page on Ycombinator"
        );
    }
}
