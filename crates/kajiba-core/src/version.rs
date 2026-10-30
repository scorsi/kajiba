//! Shared semantic version parsing for the server and CLI (ADR 0080, 0090).

/// Validate a semantic version, returning its major, minor and patch numbers.
pub fn semver(version: &str) -> Option<(u64, u64, u64)> {
    let (version, build) = version
        .split_once('+')
        .map_or((version, None), |(version, build)| (version, Some(build)));
    let (core, prerelease) = version
        .split_once('-')
        .map_or((version, None), |(core, prerelease)| {
            (core, Some(prerelease))
        });
    let numeric = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    let leading_zero = |part: &str| part.len() > 1 && part.starts_with('0');
    for (suffix, is_prerelease) in [(prerelease, true), (build, false)] {
        if let Some(suffix) = suffix {
            for part in suffix.split('.') {
                if part.is_empty()
                    || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    || (is_prerelease && numeric(part) && leading_zero(part))
                {
                    return None;
                }
            }
        }
    }
    let mut parts = core.split('.').map(|part| {
        (numeric(part) && !leading_zero(part))
            .then(|| part.parse().ok())
            .flatten()
    });
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

#[cfg(test)]
mod tests {
    use super::semver;

    #[test]
    fn validates_core_and_suffixes() {
        for version in ["1.2.3", "1.2.3-rc.1", "1.2.3-0.a-b+build.01", "1.2.3+001"] {
            assert_eq!(semver(version), Some((1, 2, 3)), "{version}");
        }
        for version in [
            "",
            "1.2",
            "1.2.3.4",
            "v1.2.3",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "+1.2.3",
            "1.2.3-",
            "1.2.3+",
            "1.2.3-+build",
            "1.2.3-rc..1",
            "1.2.3+build..1",
            "1.2.3-01",
            "1.2.3-rc.01",
            "1.2.3-rc_1",
            "1.2.3+büild",
            "1.2.3+build+other",
            "1.2.3-rc.1 ",
            "18446744073709551616.2.3",
        ] {
            assert_eq!(semver(version), None, "{version}");
        }
    }
}
