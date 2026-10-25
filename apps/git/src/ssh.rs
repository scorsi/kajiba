//! `serve-ssh`'s reading of `SSH_ORIGINAL_COMMAND` (ADR 0004, 0021). Nothing here
//! evaluates the command through a shell.

#[derive(Debug, PartialEq, Eq)]
pub enum Service {
    UploadPack,
    ReceivePack,
}

/// A Git transport command on a repository address, names validated and lowercase.
#[derive(Debug, PartialEq, Eq)]
pub struct GitCommand {
    pub service: Service,
    pub owner: String,
    pub name: String,
}

/// Accepts exactly `git-upload-pack '<path>'` or `git-receive-pack '<path>'`, the form
/// Git's SSH transport sends, where `<path>` is `<owner>/<name>` with an optional leading
/// `/` (`ssh://` URLs) and an optional `.git`. Anything else is `None`.
pub fn parse(command: &str) -> Option<GitCommand> {
    let (program, argument) = command.split_once(' ')?;
    let service = match program {
        "git-upload-pack" => Service::UploadPack,
        "git-receive-pack" => Service::ReceivePack,
        _ => return None,
    };
    // Valid names contain no quote, so a valid path needs no unquoting: any quote,
    // space or other shell character left inside fails name validation below.
    let path = argument.strip_prefix('\'')?.strip_suffix('\'')?;
    let (owner, name) = kajiba_core::names::repo_address(path)?;
    Some(GitCommand {
        service,
        owner,
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(service: Service, owner: &str, name: &str) -> Option<GitCommand> {
        Some(GitCommand {
            service,
            owner: owner.into(),
            name: name.into(),
        })
    }

    #[test]
    fn accepts_both_git_commands_and_documented_path_forms() {
        for path in [
            "alice/demo",
            "/alice/demo",
            "alice/demo.git",
            "/Alice/Demo.git",
        ] {
            assert_eq!(
                parse(&format!("git-upload-pack '{path}'")),
                command(Service::UploadPack, "alice", "demo"),
                "{path}"
            );
            assert_eq!(
                parse(&format!("git-receive-pack '{path}'")),
                command(Service::ReceivePack, "alice", "demo"),
                "{path}"
            );
        }
        // Only one `.git` is optional: the name itself must not end in it.
        assert_eq!(parse("git-upload-pack 'alice/demo.git.git'"), None);
        assert_eq!(
            parse("git-upload-pack 'a.b/c-d_e.f'"),
            command(Service::UploadPack, "a.b", "c-d_e.f")
        );
    }

    #[test]
    fn refuses_every_other_command_and_malformed_arguments() {
        for refused in [
            "",
            "git-upload-pack",
            "git-upload-pack ",
            "git-upload-archive 'alice/demo'",
            "git upload-pack 'alice/demo'",
            "git-upload-pack-x 'alice/demo'",
            "GIT-UPLOAD-PACK 'alice/demo'",
            "sh -c 'id'",
            "ls",
            "scp -t alice/demo",
            // Malformed quoting.
            "git-upload-pack alice/demo",
            "git-upload-pack \"alice/demo\"",
            "git-upload-pack 'alice/demo",
            "git-upload-pack alice/demo'",
            "git-upload-pack ''",
            "git-upload-pack 'alice/de'\\''mo'",
            "git-upload-pack  'alice/demo'",
            "git-upload-pack\t'alice/demo'",
            // Extra arguments.
            "git-upload-pack 'alice/demo' 'bob/x'",
            "git-upload-pack --strict 'alice/demo'",
            "git-upload-pack 'alice/demo' ",
            // Paths outside <owner>/<name>.
            "git-upload-pack 'alice'",
            "git-upload-pack '/alice/demo/'",
            "git-upload-pack 'alice/demo/x'",
            "git-upload-pack '//alice/demo'",
            "git-upload-pack '~alice/demo'",
            "git-upload-pack '../alice/demo'",
            "git-upload-pack 'alice/../demo'",
            "git-upload-pack '/srv/repos/x.git'",
            "git-upload-pack 'raw/demo'",
            // Shell metacharacters.
            "git-upload-pack 'alice/demo;id'",
            "git-upload-pack 'alice/$(id)'",
            "git-upload-pack 'alice/`id`'",
            "git-upload-pack 'alice/demo|id'",
            "git-upload-pack 'alice/demo&&id'",
            "git-upload-pack 'alice/demo\nid'",
            "git-upload-pack 'alice/demo\0'",
            "git-upload-pack 'alice/*'",
            "git-upload-pack 'alice/demo > x'",
        ] {
            assert_eq!(parse(refused), None, "{refused:?}");
        }
    }
}
