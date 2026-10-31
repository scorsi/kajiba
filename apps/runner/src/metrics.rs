//! The runner's Prometheus text exposition (0022): closed labels only.

use axum::{Router, routing::get};
use kajiba_proto::runner::v1::ResultState;
use std::{
    fmt::Write,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
const RESULTS: [(ResultState, &str); 3] = [
    (ResultState::Succeeded, "succeeded"),
    (ResultState::Failed, "failed"),
    (ResultState::Cancelled, "cancelled"),
];

#[derive(Default)]
pub struct Metrics {
    jobs: [AtomicU64; 3],
}

impl Metrics {
    /// An attempt this runner completed (`lost` is the server's, 0008).
    pub fn job_finished(&self, state: ResultState) {
        if let Some(index) = RESULTS.iter().position(|(result, _)| *result == state) {
            self.jobs[index].fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn exposition(&self, version: &str) -> String {
        let version = version
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        let mut out = String::from(
            "# HELP kajiba_build_info Running build version.\n# TYPE kajiba_build_info gauge\n",
        );
        writeln!(out, "kajiba_build_info{{version=\"{version}\"}} 1").unwrap();
        out.push_str("# HELP kajiba_runner_jobs_total Attempts completed by this runner.\n# TYPE kajiba_runner_jobs_total counter\n");
        for ((_, result), count) in RESULTS.iter().zip(&self.jobs) {
            writeln!(
                out,
                "kajiba_runner_jobs_total{{result=\"{result}\"}} {}",
                count.load(Ordering::Relaxed)
            )
            .unwrap();
        }
        out
    }
}

pub fn router(metrics: Arc<Metrics>) -> Router {
    Router::new().route(
        "/metrics",
        get(move || {
            let metrics = metrics.clone();
            async move {
                (
                    [(axum::http::header::CONTENT_TYPE, CONTENT_TYPE)],
                    metrics.exposition(env!("CARGO_PKG_VERSION")),
                )
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposition_counts_completed_attempts_by_result() {
        let metrics = Metrics::default();
        metrics.job_finished(ResultState::Succeeded);
        metrics.job_finished(ResultState::Succeeded);
        metrics.job_finished(ResultState::Cancelled);
        metrics.job_finished(ResultState::Unspecified);
        assert_eq!(
            metrics.exposition("0.1.0"),
            "# HELP kajiba_build_info Running build version.\n\
             # TYPE kajiba_build_info gauge\n\
             kajiba_build_info{version=\"0.1.0\"} 1\n\
             # HELP kajiba_runner_jobs_total Attempts completed by this runner.\n\
             # TYPE kajiba_runner_jobs_total counter\n\
             kajiba_runner_jobs_total{result=\"succeeded\"} 2\n\
             kajiba_runner_jobs_total{result=\"failed\"} 0\n\
             kajiba_runner_jobs_total{result=\"cancelled\"} 1\n"
        );
    }
}
