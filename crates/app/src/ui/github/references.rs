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
    if (surface == "pull" || surface == "issues") && rest.len() == 1 && positive(&target) {
        let hash = url
            .fragment()
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
        let pull = parse("https://github.com/block/buzz/pull/23#discussion_r1").unwrap();
        assert_eq!(
            pull.url,
            "https://github.com/block/buzz/pull/23#discussion_r1"
        );
        assert_eq!(pull.label, "#23");
        let commit = parse("https://github.com/block/buzz/commit/ABCDEF1234").unwrap();
        assert_eq!(commit.label, "ABCDEF1");
        assert_eq!(
            parse("https://github.com/block/buzz/").unwrap().label,
            "buzz"
        );
    }
}
