//! Fixed-cardinality Prometheus text registry (ADR 0022), following oroshi's registry.
use crate::db::events::EventBus;
use axum::{Router, http::StatusCode, routing::get};
use sqlx::SqlitePool;
use std::{
    fmt::Write,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::Duration,
};

pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
pub const HANDLERS: [&str; 4] = ["ci", "mirror", "notifier", "webhook"];
/// `runner-refused` reasons (0108).
const RUNNER_REASONS: [&str; 5] = ["unknown", "disabled", "secret", "version", "capabilities"];
/// Terminal run states counted by `kajiba_runs_total` (0022).
const RUN_RESULTS: [&str; 3] = ["succeeded", "failed", "cancelled"];
const KINDS: [&str; 6] = ["grpc", "grpc_web", "git_http", "raw", "ui", "other"];
const CODES: [&str; 22] = [
    "OK",
    "CANCELLED",
    "UNKNOWN",
    "INVALID_ARGUMENT",
    "DEADLINE_EXCEEDED",
    "NOT_FOUND",
    "ALREADY_EXISTS",
    "PERMISSION_DENIED",
    "RESOURCE_EXHAUSTED",
    "FAILED_PRECONDITION",
    "ABORTED",
    "OUT_OF_RANGE",
    "UNIMPLEMENTED",
    "INTERNAL",
    "UNAVAILABLE",
    "DATA_LOSS",
    "UNAUTHENTICATED",
    "1xx",
    "2xx",
    "3xx",
    "4xx",
    "5xx",
];
/// ADR 0020 reasons of the auth interceptor's refusals: the closed `reason` label.
pub const AUTH_REASONS: [&str; 3] = ["CREDENTIAL_INVALID", "CREDENTIAL_EXPIRED", "SCOPE_MISSING"];
const BOUNDS_MS: [u64; 6] = [10, 50, 250, 1000, 5000, 30000];
const BOUNDS_TEXT: [&str; 7] = ["0.01", "0.05", "0.25", "1", "5", "30", "+Inf"];

#[derive(Clone, Copy)]
pub enum Kind {
    Grpc,
    GrpcWeb,
    GitHttp,
    Raw,
    Ui,
    Other,
}
impl Kind {
    pub fn from_content_type(value: &str) -> Self {
        let value = value.split(';').next().unwrap_or("").trim();
        match value {
            "application/grpc-web"
            | "application/grpc-web+proto"
            | "application/grpc-web-text"
            | "application/grpc-web-text+proto" => Self::GrpcWeb,
            "application/grpc" | "application/grpc+proto" => Self::Grpc,
            _ => Self::Other,
        }
    }
}

#[derive(Default)]
struct Counter(AtomicU64);
impl Counter {
    fn add(&self, value: u64) {
        self.0
            .fetch_update(Relaxed, Relaxed, |old| Some(old.saturating_add(value)))
            .unwrap();
    }
    fn get(&self) -> u64 {
        self.0.load(Relaxed)
    }
}

#[derive(Default)]
struct Histogram {
    // Disjoint bins produce monotonic cumulative buckets and a matching count per scrape.
    bins: [Counter; 7],
    sum: AtomicU64,
}
impl Histogram {
    fn observe(&self, duration: Duration) {
        let bin = BOUNDS_MS
            .iter()
            .position(|&ms| duration <= Duration::from_millis(ms))
            .unwrap_or(6);
        self.sum
            .fetch_update(Relaxed, Relaxed, |bits| {
                Some((f64::from_bits(bits) + duration.as_secs_f64()).to_bits())
            })
            .unwrap();
        self.bins[bin].add(1);
    }
}

#[derive(Default)]
pub struct Registry {
    requests: [[Counter; 22]; 6],
    durations: [Histogram; 6],
    pushes: [Counter; 2],
    auth_refusals: [Counter; 3],
    runners_connected: AtomicU64,
    jobs_pending: AtomicU64,
    runs: [Counter; 3],
    runner_refusals: [Counter; 5],
}
impl Registry {
    /// Count one refusal of the auth interceptor; `reason` is one of `AUTH_REASONS`.
    pub fn auth_refused(&self, reason: &str) {
        if let Some(index) = AUTH_REASONS.iter().position(|known| *known == reason) {
            self.auth_refusals[index].add(1);
        }
    }

    /// Count one `runner-refused`; `reason` is one of `RUNNER_REASONS`.
    pub fn runner_refused(&self, reason: &str) {
        if let Some(index) = RUNNER_REASONS.iter().position(|known| *known == reason) {
            self.runner_refusals[index].add(1);
        }
    }

    pub fn set_runners_connected(&self, count: u64) {
        self.runners_connected.store(count, Relaxed);
    }

    /// Count one finished run; other states are ignored.
    pub fn run_finished(&self, state: &str) {
        if let Some(index) = RUN_RESULTS.iter().position(|known| *known == state) {
            self.runs[index].add(1);
        }
    }

    pub fn set_jobs_pending(&self, count: u64) {
        self.jobs_pending.store(count, Relaxed);
    }

    /// Count admission decisions once per push, regardless of its number of refs.
    pub fn push(&self, accepted: bool) {
        self.pushes[usize::from(!accepted)].add(1);
    }

    pub fn observe(&self, kind: Kind, status: &str, duration: Duration) {
        let code = status
            .parse::<u16>()
            .ok()
            .filter(|code| (100..600).contains(code))
            .map(|code| 17 + (code / 100 - 1) as usize)
            .unwrap_or_else(|| CODES.iter().position(|code| *code == status).unwrap_or(2));
        self.requests[kind as usize][code].add(1);
        self.durations[kind as usize].observe(duration);
    }

    pub fn exposition(&self, version: &str, lag: [i64; 4], failures: [u64; 4]) -> String {
        let mut out = String::new();
        let version = version
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        out.push_str(
            "# HELP kajiba_build_info Running build version.\n# TYPE kajiba_build_info gauge\n",
        );
        writeln!(out, "kajiba_build_info{{version=\"{version}\"}} 1").unwrap();
        out.push_str("# HELP kajiba_requests_total Completed requests.\n# TYPE kajiba_requests_total counter\n");
        for (kind, requests) in KINDS.iter().zip(&self.requests) {
            for (code, count) in CODES.iter().zip(requests) {
                writeln!(
                    out,
                    "kajiba_requests_total{{kind=\"{kind}\",code=\"{code}\"}} {}",
                    count.get()
                )
                .unwrap();
            }
        }
        out.push_str("# HELP kajiba_request_duration_seconds Time to complete a request.\n# TYPE kajiba_request_duration_seconds histogram\n");
        for (kind, histogram) in KINDS.iter().zip(&self.durations) {
            let mut count = 0u64;
            for (bin, bound) in histogram.bins.iter().zip(BOUNDS_TEXT) {
                count = count.saturating_add(bin.get());
                writeln!(out, "kajiba_request_duration_seconds_bucket{{kind=\"{kind}\",le=\"{bound}\"}} {count}").unwrap();
            }
            writeln!(
                out,
                "kajiba_request_duration_seconds_sum{{kind=\"{kind}\"}} {}",
                f64::from_bits(histogram.sum.load(Relaxed))
            )
            .unwrap();
            writeln!(
                out,
                "kajiba_request_duration_seconds_count{{kind=\"{kind}\"}} {count}"
            )
            .unwrap();
        }
        out.push_str("# HELP kajiba_pushes_total Push admission decisions.\n# TYPE kajiba_pushes_total counter\n");
        for (result, count) in ["accepted", "refused"].into_iter().zip(&self.pushes) {
            writeln!(
                out,
                "kajiba_pushes_total{{result=\"{result}\"}} {}",
                count.get()
            )
            .unwrap();
        }
        out.push_str("# HELP kajiba_auth_refusals_total Requests refused by the auth interceptor.\n# TYPE kajiba_auth_refusals_total counter\n");
        for (reason, count) in AUTH_REASONS.iter().zip(&self.auth_refusals) {
            writeln!(
                out,
                "kajiba_auth_refusals_total{{reason=\"{reason}\"}} {}",
                count.get()
            )
            .unwrap();
        }
        out.push_str("# HELP kajiba_runners_connected Runners with a live connection.\n# TYPE kajiba_runners_connected gauge\n");
        writeln!(
            out,
            "kajiba_runners_connected {}",
            self.runners_connected.load(Relaxed)
        )
        .unwrap();
        out.push_str("# HELP kajiba_runner_refusals_total Runner connections refused at admission.\n# TYPE kajiba_runner_refusals_total counter\n");
        for (reason, count) in RUNNER_REASONS.iter().zip(&self.runner_refusals) {
            writeln!(
                out,
                "kajiba_runner_refusals_total{{reason=\"{reason}\"}} {}",
                count.get()
            )
            .unwrap();
        }
        out.push_str("# HELP kajiba_runs_total Finished runs.\n# TYPE kajiba_runs_total counter\n");
        for (result, count) in RUN_RESULTS.iter().zip(&self.runs) {
            writeln!(
                out,
                "kajiba_runs_total{{result=\"{result}\"}} {}",
                count.get()
            )
            .unwrap();
        }
        out.push_str("# HELP kajiba_jobs_pending Jobs waiting for a runner.\n# TYPE kajiba_jobs_pending gauge\n");
        writeln!(
            out,
            "kajiba_jobs_pending {}",
            self.jobs_pending.load(Relaxed)
        )
        .unwrap();
        out.push_str("# HELP kajiba_handler_lag_events Journal events after the handler checkpoint.\n# TYPE kajiba_handler_lag_events gauge\n");
        for (handler, lag) in HANDLERS.iter().zip(lag) {
            writeln!(
                out,
                "kajiba_handler_lag_events{{handler=\"{handler}\"}} {lag}"
            )
            .unwrap();
        }
        out.push_str("# HELP kajiba_handler_failures_total Failed handler attempts.\n# TYPE kajiba_handler_failures_total counter\n");
        for (handler, failures) in HANDLERS.iter().zip(failures) {
            writeln!(
                out,
                "kajiba_handler_failures_total{{handler=\"{handler}\"}} {failures}"
            )
            .unwrap();
        }
        out
    }
}

pub fn router(registry: Arc<Registry>, readers: SqlitePool, bus: Arc<EventBus>) -> Router {
    Router::new().route(
        "/metrics",
        get(move || {
            let (registry, readers, bus) = (registry.clone(), readers.clone(), bus.clone());
            async move {
                let lag = handler_lag(&readers).await.map_err(|err| {
                    tracing::error!(event = "metrics-failed", err = %err);
                    StatusCode::SERVICE_UNAVAILABLE
                })?;
                Ok::<_, StatusCode>((
                    [(axum::http::header::CONTENT_TYPE, CONTENT_TYPE)],
                    registry.exposition(
                        env!("CARGO_PKG_VERSION"),
                        lag,
                        HANDLERS.map(|name| bus.failure_count(name)),
                    ),
                ))
            }
        }),
    )
}

pub async fn handler_lag(readers: &SqlitePool) -> Result<[i64; 4], sqlx::Error> {
    // A single read transaction keeps all handler gauges on the same journal snapshot.
    let mut transaction = readers.begin().await?;
    let mut lag = [0; 4];
    for (index, name) in HANDLERS.iter().enumerate() {
        // Ids are dense (append-only, enforced by migration 7): a difference, not a table scan.
        lag[index] = sqlx::query_scalar("SELECT MAX(0, COALESCE((SELECT MAX(id) FROM event), 0) - COALESCE((SELECT last_event_id FROM handler_checkpoint WHERE handler = ?), 0))")
            .bind(name).fetch_one(&mut *transaction).await?;
    }
    transaction.commit().await?;
    Ok(lag)
}

pub async fn bind(listen: Option<SocketAddr>) -> std::io::Result<Option<tokio::net::TcpListener>> {
    match listen {
        Some(addr) => tokio::net::TcpListener::bind(addr).await.map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests;
