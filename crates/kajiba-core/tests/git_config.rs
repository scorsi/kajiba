use kajiba_core::config::GitConfig;
use std::path::Path;

const EXAMPLE: &str = include_str!("fixtures/git.toml");
const MINIMAL: &str = "hooks_path = '/nix/store/example-kajiba-hooks'";

#[test]
fn adr_0054_configuration_loads_and_defaults_match() {
    let config = GitConfig::parse(EXAMPLE).unwrap();
    assert_eq!(config.state_dir, Path::new("/var/lib/kajiba-git"));
    assert_eq!(config.server_socket, Path::new("/run/kajiba/git.sock"));
    assert_eq!(
        config.hooks_path,
        Path::new("/nix/store/example-kajiba-hooks")
    );
    assert_eq!(config.log.level, "info");
    assert_eq!(config.metrics.listen.unwrap().to_string(), "127.0.0.1:9433");

    let defaults = GitConfig::parse(MINIMAL).unwrap();
    assert_eq!(defaults.state_dir, config.state_dir);
    assert_eq!(defaults.server_socket, config.server_socket);
    assert_eq!(defaults.hooks_path, config.hooks_path);
    assert_eq!(defaults.log.level, config.log.level);
    assert!(defaults.metrics.listen.is_none());
}

#[test]
fn invalid_configuration_names_the_field() {
    for (input, field) in [
        (String::new(), "hooks_path"),
        (format!("{MINIMAL}\ncolour = 'blue'"), "colour"),
        (format!("{MINIMAL}\n[log]\ncolour = 'blue'"), "colour"),
        (format!("{MINIMAL}\n[metrics]\ncolour = 'blue'"), "colour"),
        (
            format!("{MINIMAL}\n[log]\nlevel = 'kajiba_git=verbose'"),
            "log.level",
        ),
        (
            format!("{MINIMAL}\n[metrics]\nlisten = '0.0.0.0:9433'"),
            "metrics.listen",
        ),
        (
            format!("{MINIMAL}\n[metrics]\nlisten = '127.0.0.1:0'"),
            "metrics.listen",
        ),
    ] {
        let error = GitConfig::parse(&input).unwrap_err();
        assert!(error.contains(field), "{field}: {error}");
    }
    for field in ["state_dir", "server_socket", "hooks_path"] {
        for value in ["", "relative/path"] {
            let input = if field == "hooks_path" {
                format!("{field} = {value:?}")
            } else {
                format!("{MINIMAL}\n{field} = {value:?}")
            };
            let error = GitConfig::parse(&input).unwrap_err();
            assert!(error.contains(field), "{field}: {error}");
        }
    }
}

#[test]
fn custom_paths_and_shared_log_and_metrics_settings_load() {
    let config = GitConfig::parse(
        "state_dir = '/srv/git'\nserver_socket = '/run/custom/git.sock'\nhooks_path = '/nix/store/custom-hooks'\n[log]\nlevel = 'warn,kajiba_git=debug'\n[metrics]\nlisten = '[::1]:9433'",
    ).unwrap();
    assert_eq!(config.state_dir, Path::new("/srv/git"));
    assert_eq!(config.server_socket, Path::new("/run/custom/git.sock"));
    assert_eq!(config.hooks_path, Path::new("/nix/store/custom-hooks"));
    assert_eq!(config.log.level, "warn,kajiba_git=debug");
    assert!(config.metrics.listen.unwrap().ip().is_loopback());
}
