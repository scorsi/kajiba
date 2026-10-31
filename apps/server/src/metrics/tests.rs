use super::*;

#[test]
fn golden_exposition() {
    let registry = Registry::default();
    registry.observe(Kind::Other, "200", Duration::from_millis(250));
    registry.observe(Kind::Other, "404", Duration::from_secs(31));
    registry.observe(Kind::Grpc, "UNAVAILABLE", Duration::ZERO);
    registry.push(true);
    registry.push(false);
    registry.push(false);
    registry.auth_refused("CREDENTIAL_EXPIRED");
    registry.auth_refused("SCOPE_MISSING");
    registry.auth_refused("SCOPE_MISSING");
    // Never a label outside the closed set.
    registry.auth_refused("secret");
    registry.set_runners_connected(2);
    registry.set_jobs_pending(3);
    registry.log_truncated();
    registry.run_finished("failed");
    registry.run_finished("failed");
    registry.run_finished("succeeded");
    // A run still going is not a result.
    registry.run_finished("running");
    registry.runner_refused("secret");
    registry.runner_refused("version");
    registry.runner_refused("version");
    registry.runner_refused("CREDENTIAL_INVALID");
    assert_eq!(
        registry.exposition("test", [2, 1, 3, 3], [1, 0, 0, 0]),
        include_str!("../../tests/fixtures/metrics.prom")
    );
    assert_eq!(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8");
}

#[test]
fn histogram_boundaries_and_counter_saturation() {
    let registry = Registry::default();
    for ms in BOUNDS_MS {
        registry.observe(Kind::Grpc, "OK", Duration::from_millis(ms));
        registry.observe(
            Kind::Grpc,
            "OK",
            Duration::from_millis(ms) + Duration::from_nanos(1),
        );
    }
    registry.observe(Kind::Grpc, "OK", Duration::ZERO);
    assert_eq!(
        registry.durations[0]
            .bins
            .iter()
            .map(Counter::get)
            .collect::<Vec<_>>(),
        [2, 2, 2, 2, 2, 2, 1]
    );
    assert!(
        registry
            .exposition("test", [0; 4], [0; 4])
            .contains("kajiba_request_duration_seconds_count{kind=\"grpc\"} 13\n")
    );
    let counter = Counter::default();
    counter.add(u64::MAX);
    counter.add(1);
    assert_eq!(counter.get(), u64::MAX);
}

#[test]
fn labels_are_closed_and_version_is_escaped() {
    let registry = Registry::default();
    for (content_type, label) in [
        ("application/grpc", "grpc"),
        ("application/grpc+proto; charset=utf-8", "grpc"),
        ("application/grpc-web+proto", "grpc_web"),
        ("application/grpc-web-text", "grpc_web"),
        ("application/grpc-secret", "other"),
        ("/private?user=secret", "other"),
    ] {
        assert_eq!(KINDS[Kind::from_content_type(content_type) as usize], label);
    }
    registry.observe(Kind::Other, "secret", Duration::ZERO);
    let output = registry.exposition("a\\b\"c\nd", [0; 4], [0; 4]);
    assert!(output.contains("version=\"a\\\\b\\\"c\\nd\""));
    // `secret` is only a runner refusal reason, never an echoed label value.
    assert!(
        output
            .lines()
            .filter(|line| line.contains("secret"))
            .eq(["kajiba_runner_refusals_total{reason=\"secret\"} 0"])
    );
    for family in ["kajiba_handler_lag_events", "kajiba_handler_failures_total"] {
        let samples: Vec<_> = output
            .lines()
            .filter(|line| line.starts_with(&format!("{family}{{")))
            .collect();
        assert_eq!(samples.len(), 4);
        for (sample, handler) in samples.iter().zip(HANDLERS) {
            assert_eq!(*sample, format!("{family}{{handler=\"{handler}\"}} 0"));
        }
    }
    let samples = output.lines().filter(|line| !line.starts_with('#')).count();
    let pushes: Vec<_> = output
        .lines()
        .filter(|line| line.starts_with("kajiba_pushes_total{"))
        .collect();
    assert_eq!(
        pushes,
        [
            "kajiba_pushes_total{result=\"accepted\"} 0",
            "kajiba_pushes_total{result=\"refused\"} 0",
        ]
    );
    assert_eq!(samples, 211);
    assert!(samples <= 500);
}

#[test]
fn concurrent_updates_are_not_lost() {
    let registry = Registry::default();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..1000 {
                    registry.observe(Kind::Grpc, "OK", Duration::from_millis(250));
                    registry.push(true);
                    registry.push(false);
                }
            });
        }
    });
    assert_eq!(registry.requests[0][0].get(), 4000);
    assert_eq!(registry.pushes.map(|count| count.get()), [4000; 2]);
    assert_eq!(registry.durations[0].bins[2].get(), 4000);
    assert_eq!(
        f64::from_bits(registry.durations[0].sum.load(Relaxed)),
        1000.0
    );
}

#[tokio::test]
async fn absent_configuration_opens_no_listener() {
    let config =
        kajiba_core::config::ServerConfig::parse("[http]\npublic_url='https://example.org'")
            .unwrap();
    assert!(bind(config.metrics.listen).await.unwrap().is_none());
}
