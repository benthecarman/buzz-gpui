//! GitHub links a panel can show (`references.ts`).

/// What a GitHub link points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Repository,
    Pull,
    Issue,
    Commit,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Repository => "Repository",
            Self::Pull => "Pull request",
            Self::Issue => "Issue",
            Self::Commit => "Commit",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Reference {
    /// The canonical link, rebuilt from the parsed parts.
    pub url: String,
    /// `owner/name`.
    pub repository: String,
    pub kind: Kind,
    /// `#12`, a short commit hash or the repository name.
    pub label: String,
}

/// Fragments the panel can show (`displayableFragments`). The pull
/// request Discussion tab lists conversation comments and submitted
/// reviews; review-thread (`discussion_r…`), timeline event and other
/// anchors fall through to the browser instead of opening a panel that
/// cannot show them. Issues render only their body, so no fragment
/// qualifies.
fn displayable(surface: &str, fragment: &str) -> bool {
    if fragment.is_empty() {
        return true;
    }
    surface == "pull"
        && ["issuecomment-", "pullrequestreview-"]
            .iter()
            .any(|prefix| {
                fragment
                    .strip_prefix(prefix)
                    .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
            })
}

fn name(part: &str) -> bool {
    !part.is_empty()
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Exact host parsing: a lookalike domain or credentials can never become
/// a GitHub card.
pub fn parse(value: &str) -> Option<Reference> {
    let url = url::Url::parse(value).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let parts: Vec<&str> = url.path().split('/').filter(|p| !p.is_empty()).collect();
    let (owner, repo) = (*parts.first()?, *parts.get(1)?);
    if !name(owner) || !name(repo) {
        return None;
    }
    let repository = format!("{owner}/{repo}");
    let base = format!("https://github.com/{repository}");
    let Some(surface) = parts.get(2).copied() else {
        return Some(Reference {
            url: base,
            repository,
            kind: Kind::Repository,
            label: repo.to_string(),
        });
    };
    let rest = &parts[3..];
    let target = rest.join("/");
    let positive = |t: &str| {
        t.as_bytes()
            .first()
            .is_some_and(|b| (b'1'..=b'9').contains(b))
            && t.bytes().all(|b| b.is_ascii_digit())
    };
    let fragment = url.fragment().unwrap_or_default();
    if (surface == "pull" || surface == "issues")
        && rest.len() == 1
        && positive(&target)
        && displayable(surface, fragment)
    {
        let hash = Some(fragment)
            .filter(|f| !f.is_empty())
            .map(|f| format!("#{f}"))
            .unwrap_or_default();
        return Some(Reference {
            url: format!("{base}/{surface}/{target}{hash}"),
            repository,
            kind: if surface == "pull" {
                Kind::Pull
            } else {
                Kind::Issue
            },
            label: format!("#{target}"),
        });
    }
    if surface == "commit"
        && rest.len() == 1
        && (7..=40).contains(&target.len())
        && target.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Some(Reference {
            url: format!("{base}/commit/{target}"),
            repository,
            kind: Kind::Commit,
            label: target[..7].to_string(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_typed_references_and_refuses_lookalikes() {
        for (path, kind) in [
            ("block/buzz", Kind::Repository),
            ("block/buzz/pull/23", Kind::Pull),
            ("block/buzz/issues/12", Kind::Issue),
            ("block/buzz/commit/abcdef1234", Kind::Commit),
        ] {
            assert_eq!(
                parse(&format!("https://github.com/{path}")).map(|r| r.kind),
                Some(kind),
                "{path}"
            );
        }
        for url in [
            "https://github.com.evil.test/block/buzz/pull/23",
            "https://someone@github.com/block/buzz",
            "http://github.com/block/buzz",
            "https://github.com/block/buzz/tree/main",
            "https://github.com/block/buzz/blob/main/README.md",
            "https://github.com/block/buzz/pull/0",
            "https://github.com/block/buzz/commit/nope",
        ] {
            assert_eq!(parse(url), None, "{url}");
        }
    }

    #[test]
    fn keeps_the_fragment_on_pulls_and_shortens_commits() {
        let pull = parse("https://github.com/block/buzz/pull/23#issuecomment-1").unwrap();
        assert_eq!(
            pull.url,
            "https://github.com/block/buzz/pull/23#issuecomment-1"
        );
        assert_eq!(pull.label, "#23");
        let commit = parse("https://github.com/block/buzz/commit/ABCDEF1234").unwrap();
        assert_eq!(commit.label, "ABCDEF1");
        assert_eq!(
            parse("https://github.com/block/buzz/").unwrap().label,
            "buzz"
        );
    }

    #[test]
    fn claims_only_pull_and_issue_links_the_panel_can_show() {
        let pull = "https://github.com/block/buzz-app/pull/838";
        for fragment in ["", "#issuecomment-123", "#pullrequestreview-456"] {
            let url = format!("{pull}{fragment}");
            assert_eq!(parse(&url).map(|r| r.url), Some(url.clone()), "{url}");
        }
        let issue = "https://github.com/block/buzz-app/issues/12";
        assert_eq!(parse(issue).map(|r| r.url), Some(issue.to_string()));
        for suffix in [
            "#discussion_r4240253516",
            "#event-987",
            "#partial-timeline",
            "#issuecomment-",
            "#pullrequestreview-abc",
            "/files",
            "/files#diff-0123abcd",
            "/files#r4240253516",
            "/files/abcdef1..0123456",
            "/commits",
            "/commits/abcdef1",
            "/commits/abcdef1#r4240253516",
            "/checks",
            "/checks?check_run_id=1",
            "/conflicts",
            "/changes",
            "/activity",
        ] {
            let url = format!("{pull}{suffix}");
            assert_eq!(parse(&url), None, "{url}");
        }
        let url = format!("{issue}#issuecomment-123");
        assert_eq!(parse(&url), None, "{url}");
    }
}
