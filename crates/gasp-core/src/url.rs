use crate::error::{Error, Result};

/// Normalize a manifest URL into a clone-ready URL.
///
/// Accepts three forms:
/// - `owner/repo` shorthand → `https://{host}/{owner}/{repo}.git`
/// - full URL with `://` (e.g. `https://...`, `ssh://...`) → returned as-is
/// - SCP-style SSH `git@host:owner/repo` → returned as-is
///
/// `host` is used only for shorthand expansion.
pub fn normalize(raw: &str, host: &str, repo_name: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(Error::EmptyRepoUrl {
            name: repo_name.to_string(),
        });
    }

    // Full URL with scheme.
    if raw.contains("://") {
        return Ok(raw.to_string());
    }

    // SCP-style SSH: user@host:path
    if let Some(at_pos) = raw.find('@')
        && raw[at_pos..].contains(':')
    {
        return Ok(raw.to_string());
    }

    // Shorthand: owner/repo. Exactly one slash, no other delimiters.
    let parts: Vec<&str> = raw.split('/').collect();
    if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        let owner = parts[0];
        let repo = parts[1].strip_suffix(".git").unwrap_or(parts[1]);
        if is_safe_segment(owner) && is_safe_segment(repo) {
            return Ok(format!("https://{host}/{owner}/{repo}.git"));
        }
    }

    Err(Error::InvalidRepoUrl {
        name: repo_name.to_string(),
        url: raw.to_string(),
        reason: "not a recognized URL or `owner/repo` shorthand".to_string(),
    })
}

fn is_safe_segment(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(raw: &str) -> String {
        normalize(raw, "github.com", "test").unwrap()
    }

    #[test]
    fn shorthand_expands_to_github_https() {
        assert_eq!(n("acme/frontend"), "https://github.com/acme/frontend.git");
    }

    #[test]
    fn shorthand_strips_redundant_git_suffix() {
        assert_eq!(
            n("acme/frontend.git"),
            "https://github.com/acme/frontend.git"
        );
    }

    #[test]
    fn shorthand_respects_host() {
        let u = normalize("acme/frontend", "gitlab.example.com", "test").unwrap();
        assert_eq!(u, "https://gitlab.example.com/acme/frontend.git");
    }

    #[test]
    fn full_https_passes_through() {
        let raw = "https://github.com/acme/frontend.git";
        assert_eq!(n(raw), raw);
    }

    #[test]
    fn full_ssh_url_passes_through() {
        let raw = "ssh://git@github.com/acme/frontend.git";
        assert_eq!(n(raw), raw);
    }

    #[test]
    fn scp_style_ssh_passes_through() {
        let raw = "git@github.com:acme/frontend.git";
        assert_eq!(n(raw), raw);
    }

    #[test]
    fn empty_url_rejected() {
        let err = normalize("", "github.com", "lib").unwrap_err();
        assert!(matches!(err, Error::EmptyRepoUrl { name } if name == "lib"));
    }

    #[test]
    fn too_many_slashes_rejected() {
        let err = normalize("a/b/c", "github.com", "lib").unwrap_err();
        assert!(matches!(err, Error::InvalidRepoUrl { .. }));
    }

    #[test]
    fn single_segment_rejected() {
        let err = normalize("oops", "github.com", "lib").unwrap_err();
        assert!(matches!(err, Error::InvalidRepoUrl { .. }));
    }

    #[test]
    fn unsafe_characters_rejected() {
        let err = normalize("acme/repo space", "github.com", "lib").unwrap_err();
        assert!(matches!(err, Error::InvalidRepoUrl { .. }));
    }

    #[test]
    fn whitespace_is_trimmed() {
        assert_eq!(
            n("  acme/frontend  "),
            "https://github.com/acme/frontend.git"
        );
    }
}
