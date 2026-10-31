//! nisshi schema v1 on stderr (ADR 0022).

use std::fmt;
use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

pub fn filter(level: &str, rust_log: Option<&str>) -> Result<Targets, String> {
    let configured = level
        .parse::<Targets>()
        .map_err(|e| format!("log.level: {e}"))?;
    Ok(rust_log.and_then(|s| s.parse().ok()).unwrap_or(configured))
}

pub fn init(level: &str) -> Result<(), String> {
    let rust_log = std::env::var("RUST_LOG").ok();
    let targets = filter(level, rust_log.as_deref())?;
    tracing_subscriber::registry()
        .with(
            JsonLayer.with_filter(tracing_subscriber::filter::filter_fn(move |metadata| {
                // Correlation spans must survive even when info events are filtered out.
                metadata.is_span() || targets.would_enable(metadata.target(), metadata.level())
            })),
        )
        .try_init()
        .map_err(|e| e.to_string())
}

pub fn install_panic_hook() {
    // The payload is not logged: a panic message may carry data (ADR 0022: never a secret).
    std::panic::set_hook(Box::new(|info| {
        let at = info
            .location()
            .map_or_else(String::new, |at| format!(" at {}:{}", at.file(), at.line()));
        error("fatal", &format!("panic{at}"))
    }));
}

pub struct JsonLayer;

#[derive(Default)]
struct RequestId(Option<String>);

impl Visit for RequestId {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "request_id" {
            self.0 = Some(format!("{value:?}"));
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "request_id" {
            self.0 = Some(value.into());
        }
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for JsonLayer {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::Id,
        ctx: Context<'_, S>,
    ) {
        let mut request_id = RequestId::default();
        attrs.record(&mut request_id);
        ctx.span(id).unwrap().extensions_mut().insert(request_id);
    }

    fn on_record(&self, id: &tracing::Id, values: &tracing::span::Record<'_>, ctx: Context<'_, S>) {
        let span = ctx.span(id).unwrap();
        let mut extensions = span.extensions_mut();
        values.record(extensions.get_mut::<RequestId>().unwrap());
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let request_id = ctx.event_scope(event).and_then(|mut scope| {
            scope.find_map(|span| {
                span.extensions()
                    .get::<RequestId>()
                    .and_then(|id| id.0.clone())
            })
        });
        if let Some(line) = format(event, &ts_now(), request_id.as_deref()) {
            write(&mut io::stderr().lock(), &line);
        }
    }
}

// Like kekkai's layer, separate envelope fields from the event's fields.
#[derive(Default, Serialize)]
struct Fields {
    #[serde(skip)]
    event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    err: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Serialize)]
struct Line<'a> {
    v: u8,
    ts: &'a str,
    level: &'a str,
    event: &'a str,
    #[serde(flatten)]
    fields: &'a Fields,
}

fn format(event: &Event<'_>, ts: &str, request_id: Option<&str>) -> Option<String> {
    let mut fields = Fields::default();
    event.record(&mut fields);
    if let Some(request_id) = request_id {
        fields.extra.insert("request_id".into(), request_id.into());
    }
    let level = match *event.metadata().level() {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        _ => "debug",
    };
    if !matches!(level, "warn" | "error") {
        fields.err = None;
    }
    // Dependencies' unnamed events cannot be assigned an invented ADR event.
    let name = fields.event.as_deref()?;
    if !event_name(name) {
        return None;
    }
    render(ts, level, name, &fields)
}

fn event_name(name: &str) -> bool {
    name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && name.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fn render(ts: &str, level: &str, event: &str, fields: &Fields) -> Option<String> {
    let line = serde_json::to_string(&Line {
        v: 1,
        ts,
        level,
        event,
        fields,
    })
    .ok()?;
    // nisshi refuses service lines larger than 1 MiB.
    (line.len() <= 1_048_576).then_some(line)
}

pub fn error(event: &str, err: &str) {
    if !event_name(event) {
        return;
    }
    let mut fields = Fields {
        err: Some(err.into()),
        ..Fields::default()
    };
    let request_id = tracing::Span::current()
        .with_subscriber(|(id, dispatch)| {
            let registry = dispatch.downcast_ref::<tracing_subscriber::Registry>()?;
            registry.span(id)?.scope().find_map(|span| {
                span.extensions()
                    .get::<RequestId>()
                    .and_then(|id| id.0.clone())
            })
        })
        .flatten();
    if let Some(request_id) = request_id {
        fields.extra.insert("request_id".into(), request_id.into());
    }
    if let Some(line) = render(&ts_now(), "error", event, &fields) {
        write(&mut io::stderr().lock(), &line);
    }
}

fn write(writer: &mut impl Write, line: &str) {
    // A closed stderr must not panic, especially inside the panic hook.
    let _ = writeln!(writer, "{line}");
}

impl Fields {
    fn record(&mut self, field: &Field, value: Value) {
        match field.name() {
            "event" => self.event = Some(text(value)),
            "message" | "msg" => self.msg = Some(text(value)),
            "err" => self.err = Some(text(value)),
            "v" | "ts" | "level" | "node" | "service" | "stream" | "seq" | "source" | "recv_ts" => {
            }
            name if name.starts_with('_') => {}
            name => {
                self.extra.insert(name.into(), value);
            }
        }
    }
}

fn text(value: Value) -> String {
    match value {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.record(field, value.into());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.record_str(field, &format!("{value:?}"));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record(field, value.into());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record(field, value.into());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record(field, value.into());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.record(field, value.into());
    }
}

fn ts_now() -> String {
    let ns = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
    .clamp(i64::MIN as i128, i64::MAX as i128);
    ts_from(
        ns.div_euclid(1_000_000_000) as i64,
        ns.rem_euclid(1_000_000_000) as u32,
    )
}

/// civil_from_days (Howard Hinnant), from nisshi/docs/log-schema.md and hikyaku/src/ts.rs.
pub(crate) fn ts_from(secs: i64, nanos: u32) -> String {
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{nanos:09}Z",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    mod schema {
        include!("../tests/fixtures/log_schema.rs");
    }

    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl<S: Subscriber> Layer<S> for Capture {
        fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
            if let Some(line) = format(event, &ts_from(0, 0), None) {
                write(&mut *self.0.lock().unwrap(), &line);
            }
        }
    }

    fn capture(targets: Targets, emit: impl FnOnce()) -> Vec<String> {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let subscriber =
            tracing_subscriber::registry().with(Capture(bytes.clone()).with_filter(targets));
        tracing::subscriber::with_default(subscriber, emit);
        let lines: Vec<String> = String::from_utf8(bytes.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert!(lines.iter().all(|line| schema::valid(line)));
        lines
    }

    #[test]
    fn timestamps() {
        for (secs, nanos, expected) in [
            (0, 0, "1970-01-01T00:00:00.000000000Z"),
            (1_709_164_800, 1, "2024-02-29T00:00:00.000000001Z"),
            (1_767_225_599, 999_999_999, "2025-12-31T23:59:59.999999999Z"),
            (-1, 999_999_999, "1969-12-31T23:59:59.999999999Z"),
            (
                -9_223_372_037,
                145_224_192,
                "1677-09-21T00:12:43.145224192Z",
            ),
            (9_223_372_036, 854_775_807, "2262-04-11T23:47:16.854775807Z"),
            (4_107_542_400, 0, "2100-03-01T00:00:00.000000000Z"),
        ] {
            assert_eq!(ts_from(secs, nanos), expected);
        }
        assert_eq!(ts_now().len(), 30);
    }

    #[test]
    fn escaping_types_and_envelope_order() {
        let special = "a\"b\\c\n\r\t\u{0}é 日誌";
        let lines = capture("trace".parse().unwrap(), || {
            tracing::warn!(event = "config-invalid", err = %special, count = 3u64,
                signed = -2i64, enabled = true, ratio = 0.5, "message {special}");
        });
        assert_eq!(lines.len(), 1);
        let v: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(v["err"], special);
        assert_eq!(v["msg"], format!("message {special}"));
        assert_eq!(v["count"], 3);
        assert_eq!(v["signed"], -2);
        assert_eq!(v["enabled"], true);
        assert_eq!(v["ratio"], 0.5);
        assert!(schema::valid(&lines[0]));
        assert!(lines[0].starts_with("{\"v\":1,\"ts\":\"1970-01-01T00:00:00.000000000Z\",\"level\":\"warn\",\"event\":\"config-invalid\",\"msg\":"));
        assert!(lines[0].find("\"msg\"").unwrap() < lines[0].find("\"err\"").unwrap());
        assert!(lines[0].find("\"err\"").unwrap() < lines[0].find("\"count\"").unwrap());
    }

    #[test]
    fn levels_and_err_restriction() {
        let lines = capture("trace".parse().unwrap(), || {
            tracing::trace!(event = "request", err = "hidden");
            tracing::debug!(event = "request", err = "hidden");
            tracing::info!(event = "start", err = "hidden");
            tracing::warn!(event = "push-refused", err = "warning");
            tracing::error!(event = "fatal", err = "error");
        });
        assert_eq!(lines.len(), 5);
        for (line, expected) in lines
            .iter()
            .zip(["debug", "debug", "info", "warn", "error"])
        {
            assert!(schema::valid(line), "{line}");
            let v: Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["level"], expected);
            assert_eq!(v.get("err").is_some(), matches!(expected, "warn" | "error"));
        }
    }

    #[test]
    fn reserved_fields_cannot_override_envelope() {
        let lines = capture("trace".parse().unwrap(), || {
            tracing::info!(
                event = "start",
                node = "x",
                service = "x",
                stream = "x",
                seq = 1,
                source = "x",
                recv_ts = "x",
                _hidden = true,
                v = 2,
                ts = "bad",
                level = "error",
                version = "0.1.0"
            );
        });
        assert_eq!(lines.len(), 1);
        assert!(schema::valid(&lines[0]));
        let v: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["level"], "info");
        assert_eq!(v["version"], "0.1.0");
        assert_eq!(v.as_object().unwrap().len(), 5);
    }

    #[test]
    fn target_filter_and_precedence() {
        let emit = || {
            tracing::debug!(target: "kajiba", event = "request");
            tracing::info!(target: "kajiba", event = "start");
            tracing::info!(target: "dependency", event = "request");
            tracing::warn!(target: "dependency", event = "request");
        };
        let lines = capture(filter("error", Some("warn,kajiba=debug")).unwrap(), emit);
        let levels: Vec<Value> = lines
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["level"].clone())
            .collect();
        assert_eq!(levels, ["debug", "info", "warn"]);
        assert_eq!(capture(filter("info", None).unwrap(), emit).len(), 3);
        assert_eq!(
            capture(filter("error", Some("kajiba=verbose")).unwrap(), emit).len(),
            0
        );
        assert!(
            filter("kajiba=verbose", Some("info"))
                .unwrap_err()
                .contains("log.level")
        );
        assert!(capture(filter("off", None).unwrap(), emit).is_empty());
    }

    #[test]
    fn unnamed_malformed_and_oversized_events_are_dropped() {
        let lines = capture("trace".parse().unwrap(), || {
            tracing::info!("unnamed");
            tracing::info!(event = "bad name");
            tracing::info!(event = "start", msg = "x".repeat(1_048_576));
        });
        assert!(lines.is_empty());
        for name in ["", "-start", "start-", "start--end", "Start", "1start"] {
            assert!(!event_name(name));
        }
    }

    #[test]
    fn broken_stderr_does_not_panic() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        write(&mut Closed, "{}");
    }

    #[test]
    fn panic_hook_writes_fatal_even_when_filtered_off() {
        const CHILD: &str = "KAJIBA_TEST_PANIC";
        if std::env::var_os(CHILD).is_some() {
            install_panic_hook();
            init("off").unwrap();
            panic!("panic with a quote \" and newline\n");
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "log::tests::panic_hook_writes_fatal_even_when_filtered_off",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("RUST_LOG", "off")
            .output()
            .unwrap();
        assert!(!output.status.success());
        let text = String::from_utf8(output.stderr).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        let line = text.trim_end_matches('\n');
        assert!(schema::valid(line), "{line}");
        let v: Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["event"], "fatal");
        assert_eq!(v["level"], "error");
        // The location is kept; the payload, which could carry data, is not.
        let err = v["err"].as_str().unwrap();
        assert!(
            err.starts_with("panic at ") && err.contains("log.rs:"),
            "{err}"
        );
        assert!(!err.contains("quote"), "{err}");
    }
}

#[cfg(test)]
mod correlation_tests {
    use super::*;

    #[test]
    fn request_spans_cover_nested_events_records_and_direct_errors() {
        const CHILD: &str = "KAJIBA_TEST_LOG_SPAN";
        if std::env::var_os(CHILD).is_some() {
            init(&std::env::var(CHILD).unwrap()).unwrap();
            let request = tracing::info_span!("request", request_id = tracing::field::Empty);
            request.record("request_id", "outer");
            request.in_scope(|| {
                let child = tracing::info_span!("handler");
                child.in_scope(|| {
                    tracing::info!(event = "handling", request_id = "cannot-override");
                    error("request-failed", "direct error");
                });
                tracing::info_span!("nested-request", request_id = "inner").in_scope(|| {
                    tracing::info!(event = "handled");
                });
            });
            tracing::info!(event = "stopping");
            return;
        }
        for level in ["info", "error", "off"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "log::correlation_tests::request_spans_cover_nested_events_records_and_direct_errors", "--nocapture"])
                .env(CHILD, level).env_remove("RUST_LOG").output().unwrap();
            assert!(output.status.success(), "{output:?}");
            let text = String::from_utf8(output.stderr).unwrap();
            let lines: Vec<Value> = text
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            if level == "info" {
                assert_eq!(lines.len(), 4, "{text}");
                assert_eq!(lines[0]["request_id"], "outer");
                assert_eq!(lines[1]["request_id"], "outer");
                assert_eq!(lines[2]["request_id"], "inner");
                assert!(lines[3].get("request_id").is_none());
            } else {
                assert_eq!(lines.len(), 1, "{text}");
                assert_eq!(lines[0]["request_id"], "outer");
            }
        }
    }
}
