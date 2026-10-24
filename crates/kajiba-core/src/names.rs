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

#[cfg(test)]
mod tests {
    use super::{NameError, validate_name};

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
}
