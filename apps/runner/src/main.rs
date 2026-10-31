mod cache;
mod host;
mod metrics;
mod session;
mod work;

use clap::{Parser, Subcommand};
use kajiba_core::{config::RunnerConfig, log};
use kajiba_proto::runner::v1::Capabilities;
use std::{path::PathBuf, process::ExitCode, sync::Arc};
use tonic::transport::{ClientTlsConfig, Endpoint};

/// kajiba-runner — connects to a kajiba-server and executes jobs.
#[derive(Debug, Parser)]
#[command(version, args_conflicts_with_subcommands = true)]
struct Args {
    /// Configuration file: run the runner.
    #[arg(long)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum Command {
    /// Inspect configuration offline.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum ConfigCommand {
    /// Validate configuration without contacting the forge.
    Check {
        /// Also open the SVID, key and bundle files.
        #[arg(long)]
        files: bool,
        path: PathBuf,
    },
}

fn load_config(path: &std::path::Path, files: bool) -> Result<RunnerConfig, String> {
    let config =
        kajiba_core::config::read_config(path).and_then(|input| RunnerConfig::parse(&input))?;
    if files {
        config.check_files()?;
    }
    Ok(config)
}

/// The configured hostname, else the OS's (0019 as amended).
fn hostname(configured: Option<&str>) -> Result<String, String> {
    if let Some(hostname) = configured {
        return Ok(hostname.to_owned());
    }
    let mut buffer = [0_u8; 256];
    // SAFETY: the buffer is writable for its whole length.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } != 0 {
        return Err(format!("hostname: {}", std::io::Error::last_os_error()));
    }
    let end = buffer
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(buffer.len());
    Ok(String::from_utf8_lossy(&buffer[..end]).into_owned())
}

/// The two halves of this build's Nix system name (Rust's `macos` is Nix's `darwin`).
fn system() -> (&'static str, &'static str) {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        os => os,
    };
    (std::env::consts::ARCH, os)
}

fn capabilities(config: &RunnerConfig) -> Capabilities {
    let (arch, os) = system();
    Capabilities {
        arch: arch.into(),
        os: os.into(),
        backends: config
            .backends
            .iter()
            .filter_map(|backend| Some(serde_json::to_value(backend).ok()?.as_str()?.to_owned()))
            .collect(),
        tags: config.tags.clone(),
        max_jobs: config.max_jobs,
    }
}

// Exit codes: 0 success (including help/version), 1 runtime error, 2 usage error.
#[tokio::main]
async fn main() -> ExitCode {
    log::install_panic_hook();
    let (path, files) = match Args::try_parse() {
        Ok(Args {
            command:
                Some(Command::Config {
                    command: ConfigCommand::Check { files, path },
                }),
            ..
        }) => (path, Some(files)),
        Ok(Args {
            config: Some(path), ..
        }) => (path, None),
        Ok(_) => {
            log::error("fatal", "missing --config; use --help");
            return ExitCode::from(2);
        }
        Err(err) if err.use_stderr() => {
            log::error("fatal", "invalid command line; use --help");
            return ExitCode::from(2);
        }
        Err(err) => {
            print!("{err}");
            return ExitCode::SUCCESS;
        }
    };
    let config = match load_config(&path, files.unwrap_or(true)) {
        Ok(config) => config,
        Err(err) => {
            log::error("config-invalid", &err);
            return ExitCode::FAILURE;
        }
    };
    if files.is_some() {
        return ExitCode::SUCCESS;
    }
    if let Err(err) = log::init(&config.log.level) {
        log::error("fatal", &err);
        return ExitCode::FAILURE;
    }
    match serve(config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            log::error("fatal", &err);
            ExitCode::FAILURE
        }
    }
}

async fn serve(config: RunnerConfig) -> Result<(), String> {
    tracing::info!(event = "start", version = env!("CARGO_PKG_VERSION"));
    let _state_lock = host::prepare_state(&config.state_dir)?;
    host::check(&config.tls.key_file).await?;
    // shortcut: system roots and no client certificate, until the SVID connector (milestones' Runner mTLS, task 7).
    let endpoint = Endpoint::from_shared(config.url.clone())
        .map_err(|err| format!("url: {err}"))?
        .tls_config(ClientTlsConfig::new().with_native_roots())
        .map_err(|err| format!("url: TLS: {err}"))?;
    let identity = session::Identity {
        name: config.name.clone(),
        hostname: hostname(config.hostname.as_deref())?,
        capabilities: capabilities(&config),
    };
    let metrics = Arc::new(metrics::Metrics::default());
    if let Some(listen) = config.metrics.listen {
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .map_err(|err| format!("metrics.listen: {err}"))?;
        tracing::info!(event = "metrics-listening", addr = %listen);
        let app = metrics::router(metrics.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
    }
    let (jobs, assigned) = tokio::sync::mpsc::channel(16);
    let attempts = session::Attempts::default();
    let client = kajiba_proto::runner::v1::runner_service_client::RunnerServiceClient::new(
        endpoint.connect_lazy(),
    );
    let worker = work::Worker {
        attempts: attempts.clone(),
        cache: Arc::new(cache::Cache::new(&config.state_dir, client)),
        executor: kajiba_exec::Nix::default(),
        metrics,
        max_jobs: config.max_jobs as usize,
    };
    tokio::spawn(work::serve(worker, assigned));
    tracing::info!(event = "ready");
    let connection = session::run(endpoint, identity, attempts, jobs, tokio::time::sleep);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|err| err.to_string())?;
    tokio::select! {
        () = connection => {}
        _ = terminate.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    tracing::info!(event = "stopping");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/kajiba-core/tests/fixtures/runner.toml"
    );

    #[test]
    fn parses_the_daemon_and_config_check() {
        let args =
            Args::try_parse_from(["kajiba-runner", "--config", "/etc/kajiba/runner.toml"]).unwrap();
        assert_eq!(
            (args.config.as_deref(), args.command),
            (Some(std::path::Path::new("/etc/kajiba/runner.toml")), None)
        );
        for files in [false, true] {
            let mut argv = vec!["kajiba-runner", "config", "check"];
            if files {
                argv.push("--files");
            }
            argv.push("/etc/kajiba/runner.toml");
            assert_eq!(
                Args::try_parse_from(argv).unwrap().command,
                Some(Command::Config {
                    command: ConfigCommand::Check {
                        files,
                        path: "/etc/kajiba/runner.toml".into()
                    }
                })
            );
        }
    }

    #[test]
    fn config_check_loads_the_adr_example() {
        let config = load_config(EXAMPLE.as_ref(), false).unwrap();
        assert_eq!(config.name, "build-1");
        // `--files` opens the SVID files, absent here.
        assert!(
            load_config(EXAMPLE.as_ref(), true)
                .unwrap_err()
                .starts_with("tls.svid_file")
        );
    }

    #[test]
    fn hostname_defaults_to_the_os_and_can_be_overridden() {
        assert_eq!(
            hostname(Some("vm-host.example.org")).unwrap(),
            "vm-host.example.org"
        );
        let os = std::process::Command::new("uname")
            .arg("-n")
            .output()
            .unwrap();
        assert_eq!(
            hostname(None).unwrap(),
            String::from_utf8(os.stdout).unwrap().trim()
        );
    }

    #[test]
    fn capabilities_name_the_build_target_as_nix_does() {
        let config = load_config(EXAMPLE.as_ref(), false).unwrap();
        let announced = capabilities(&config);
        assert_eq!(announced.backends, ["nix"]);
        assert_eq!(announced.max_jobs, 1);
        assert!(["x86_64", "aarch64"].contains(&announced.arch.as_str()));
        assert!(["linux", "darwin"].contains(&announced.os.as_str()));
    }
}
