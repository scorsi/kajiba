//! Name validation shared by configuration and domain entry points.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    Length,
    InvalidCharacters,
    GitSuffix,
    Reserved,
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Length => "name must contain 1 to 63 ASCII characters",
            Self::InvalidCharacters => {
                "name must start with an ASCII letter or digit and contain only ASCII letters, digits, '.', '_' or '-'"
            }
            Self::GitSuffix => "name must not end in .git",
            Self::Reserved => "name is reserved for a route",
        })
    }
}

impl std::error::Error for NameError {}

/// Validates an owner or repository name under ADR 0021 and returns lowercase.
/// Runner and credential names use the owner rule because ADR 0019 defines no
/// separate syntax for them.
pub fn validate_name(name: &str) -> Result<String, NameError> {
    if !(1..=63).contains(&name.len()) {
        return Err(NameError::Length);
    }
    let name = name.to_ascii_lowercase();
    if !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
    {
        return Err(NameError::InvalidCharacters);
    }
    if name.ends_with(".git") {
        return Err(NameError::GitSuffix);
    }
    // Every gRPC package (`kajiba.v1`, `kajiba.runner.v1`, …) is a top-level route (0021),
    // as is server reflection under `grpc.reflection.*`.
    if name.starts_with("kajiba.")
        || name.starts_with("grpc.")
        || matches!(
            name.as_str(),
            "raw" | "healthz" | "readyz" | "auth" | "metrics"
        )
    {
        return Err(NameError::Reserved);
    }
    Ok(name)
}

/// Why a ref name breaks a `git check-ref-format` rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefError {
    OneLevel,
    Component,
    Character,
    Slash,
    Sequence,
    TrailingDot,
    At,
}

impl fmt::Display for RefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OneLevel => "ref name must contain a '/'",
            Self::Component => "ref name component must not be empty, start with '.' or end with '.lock'",
            Self::Character => {
                "ref name must not contain control characters, space, '~', '^', ':', '?', '*', '[' or '\\'"
            }
            Self::Slash => "ref name must not start or end with '/'",
            Self::Sequence => "ref name must not contain '..' or '@{'",
            Self::TrailingDot => "ref name must not end with '.'",
            Self::At => "ref name must not be '@'",
        })
    }
}

impl std::error::Error for RefError {}

/// Validates a full ref name (`refs/heads/main`) under `git check-ref-format`
/// without options (0021). Branch names are checked as `refs/heads/<branch>`.
pub fn validate_ref(name: &str) -> Result<(), RefError> {
    if name == "@" {
        return Err(RefError::At);
    }
    if name.bytes().any(|c| {
        c < 0x20
            || matches!(
                c,
                0x7f | b' ' | b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\'
            )
    }) {
        return Err(RefError::Character);
    }
    if name.starts_with('/') || name.ends_with('/') {
        return Err(RefError::Slash);
    }
    if name.contains("..") || name.contains("@{") {
        return Err(RefError::Sequence);
    }
    if name.ends_with('.') {
        return Err(RefError::TrailingDot);
    }
    if name
        .split('/')
        .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with(".lock"))
    {
        return Err(RefError::Component);
    }
    if !name.contains('/') {
        return Err(RefError::OneLevel);
    }
    Ok(())
}

/// Why a path inside a repository tree is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathError {
    EmptySegment,
    ParentSegment,
    Nul,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmptySegment => "path must not contain an empty segment",
            Self::ParentSegment => "path must not contain a '..' segment",
            Self::Nul => "path must not contain NUL",
        })
    }
}

impl std::error::Error for PathError {}

/// Validates a `/`-separated path inside a repository tree (0021).
pub fn validate_path(path: &str) -> Result<(), PathError> {
    if path.contains('\0') {
        return Err(PathError::Nul);
    }
    for segment in path.split('/') {
        match segment {
            "" => return Err(PathError::EmptySegment),
            ".." => return Err(PathError::ParentSegment),
            _ => {}
        }
    }
    Ok(())
}

/// Canonical uppercase ULID, shared by the SSH context and daemon generations.
pub fn is_public_id(value: &str) -> bool {
    value.len() == 26
        && value.as_bytes()[0] <= b'7'
        && value
            .bytes()
            .all(|byte| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::{NameError, PathError, RefError, validate_name, validate_path, validate_ref};

    #[test]
    fn valid_names() {
        for name in [
            "alice",
            "demo",
            "build-1",
            "github",
            "0",
            "a.b_c-d",
            "a.",
            "a_",
            "a-",
            "a..b",
            "raw-data",
            "grpc",
            "grpc-tools",
        ] {
            assert_eq!(validate_name(name), Ok(name.to_owned()), "{name:?}");
        }
    }

    #[test]
    fn normalizes_case() {
        for (name, expected) in [("ALICE", "alice"), ("My.Repo_1-X", "my.repo_1-x")] {
            assert_eq!(validate_name(name), Ok(expected.to_owned()));
        }
    }

    #[test]
    fn rejects_invalid_characters() {
        for name in [
            ".alice", "_alice", "-alice", "a/b", "a\\b", "a b", " a", "a ", "a\n", "a\0b", "a:b",
            "a@b", "a%b", "a?b", "a#b", "aéb", "é", "😀",
        ] {
            assert_eq!(
                validate_name(name),
                Err(NameError::InvalidCharacters),
                "{name:?}"
            );
        }
    }

    #[test]
    fn length_boundaries() {
        // The minimum minus one is empty; the maximum plus one is 64.
        for length in [0, 1, 2, 62, 63, 64] {
            let name = "a".repeat(length);
            let expected = if (1..=63).contains(&length) {
                Ok(name.clone())
            } else {
                Err(NameError::Length)
            };
            assert_eq!(validate_name(&name), expected, "length {length}");
        }
    }

    #[test]
    fn rejects_git_suffix_after_normalization() {
        for name in ["a.git", "repo.GIT", "Repo.GiT", "a.git.git"] {
            assert_eq!(validate_name(name), Err(NameError::GitSuffix), "{name:?}");
        }
        for name in ["git", "a.gitx", "a.git.more"] {
            assert_eq!(validate_name(name), Ok(name.to_owned()), "{name:?}");
        }
    }

    #[test]
    fn rejects_every_reserved_name_after_normalization() {
        for name in [
            "raw",
            "kajiba.v1",
            "kajiba.runner.v1",
            "kajiba.admin.v1",
            "kajiba.v1.extra",
            "grpc.reflection.v1",
            "grpc.health.v1",
            "healthz",
            "readyz",
            "auth",
            "metrics",
        ] {
            assert_eq!(validate_name(name), Err(NameError::Reserved), "{name:?}");
            assert_eq!(
                validate_name(&name.to_ascii_uppercase()),
                Err(NameError::Reserved),
                "{name:?} uppercase"
            );
        }
    }

    #[test]
    fn valid_refs() {
        for name in [
            "refs/heads/main",
            "refs/heads/feature/x",
            "refs/tags/v1.0",
            "refs/heads/a.b",
            "refs/heads/a.lockx",
            "refs/heads/-x",
            "refs/heads/@",
            "refs/heads/a@b",
            "refs/heads/é",
            "a/b",
        ] {
            assert_eq!(validate_ref(name), Ok(()), "{name:?}");
        }
    }

    #[test]
    fn rejects_invalid_refs() {
        for (name, err) in [
            ("", RefError::Component),
            ("main", RefError::OneLevel),
            ("HEAD", RefError::OneLevel),
            ("@", RefError::At),
            ("refs/heads/.x", RefError::Component),
            ("refs/heads/x.lock", RefError::Component),
            ("refs/heads/.lock", RefError::Component),
            ("refs/heads/a//b", RefError::Component),
            ("refs/heads/a..b", RefError::Sequence),
            ("refs/heads/a@{1}", RefError::Sequence),
            ("refs/heads/a.", RefError::TrailingDot),
            ("/refs/heads/a", RefError::Slash),
            ("refs/heads/a/", RefError::Slash),
            ("refs/heads/a b", RefError::Character),
            ("refs/heads/a\0b", RefError::Character),
            ("refs/heads/a\nb", RefError::Character),
            ("refs/heads/a\x1fb", RefError::Character),
            ("refs/heads/a\x7fb", RefError::Character),
            ("refs/heads/a~1", RefError::Character),
            ("refs/heads/a^", RefError::Character),
            ("refs/heads/a:b", RefError::Character),
            ("refs/heads/a?", RefError::Character),
            ("refs/heads/a*", RefError::Character),
            ("refs/heads/a[b", RefError::Character),
            ("refs/heads/a\\b", RefError::Character),
        ] {
            assert_eq!(validate_ref(name), Err(err), "{name:?}");
        }
    }

    #[test]
    fn valid_paths() {
        for path in [
            "README.md",
            "a/b/c",
            ".hidden",
            "a/./b",
            "a..b",
            "...",
            "é/😀",
        ] {
            assert_eq!(validate_path(path), Ok(()), "{path:?}");
        }
    }

    #[test]
    fn rejects_invalid_paths() {
        for (path, err) in [
            ("", PathError::EmptySegment),
            ("/", PathError::EmptySegment),
            ("/a", PathError::EmptySegment),
            ("a/", PathError::EmptySegment),
            ("a//b", PathError::EmptySegment),
            ("..", PathError::ParentSegment),
            ("a/../b", PathError::ParentSegment),
            ("a/..", PathError::ParentSegment),
            ("a\0b", PathError::Nul),
        ] {
            assert_eq!(validate_path(path), Err(err), "{path:?}");
        }
    }
}
