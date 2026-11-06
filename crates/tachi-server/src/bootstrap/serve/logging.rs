pub(super) fn primary_log_path(app_home: &std::path::Path) -> std::path::PathBuf {
    app_home.join("logs").join("tachi.log")
}

fn oauth_log_allowed(metadata: &tracing::Metadata<'_>) -> bool {
    // rmcp 3.8 logs authorization codes and token responses at DEBUG. This
    // independent filter cannot be relaxed by RUST_LOG or span directives.
    !metadata.target().starts_with("rmcp::transport::auth")
        || *metadata.level() <= tracing::Level::INFO
}

pub(super) fn init_tracing(app_home: &std::path::Path) {
    use std::io::Write as _;
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tachi_server=info,rmcp=warn"));

    // Try the configured app home first, then /tmp, then bare stderr.
    let primary = primary_log_path(app_home);
    let fallback = std::path::PathBuf::from("/tmp/tachi.log");

    let opener = |path: &std::path::Path| -> Option<std::fs::File> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)
                .ok()
        }
        #[cfg(not(unix))]
        {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
        }
    };

    let (writer, sink_label): (Box<dyn std::io::Write + Send + Sync>, String) =
        if let Some(file) = opener(&primary) {
            (Box::new(file), primary.display().to_string())
        } else if let Some(file) = opener(&fallback) {
            (Box::new(file), fallback.display().to_string())
        } else {
            (Box::new(std::io::stderr()), "stderr".to_string())
        };

    // Wrap the writer behind Mutex so MakeWriter can hand out shared refs.
    let shared: std::sync::Arc<std::sync::Mutex<Box<dyn std::io::Write + Send + Sync>>> =
        std::sync::Arc::new(std::sync::Mutex::new(writer));

    struct SharedWriter(std::sync::Arc<std::sync::Mutex<Box<dyn std::io::Write + Send + Sync>>>);
    impl std::io::Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::Other, "log writer mutex poisoned")
                })?
                .write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0
                .lock()
                .map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::Other, "log writer mutex poisoned")
                })?
                .flush()
        }
    }

    let make_writer = move || SharedWriter(shared.clone());

    let file_layer = fmt::Layer::new()
        .with_writer(make_writer)
        .with_ansi(false)
        .with_target(true);

    // Best-effort registration. A second call (e.g. from a test) becomes a
    // no-op because `set_global_default` was already set.
    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::filter::filter_fn(oauth_log_allowed))
        .with(file_layer)
        .try_init();

    // Mirror the sink choice to stderr so operators can find their logs.
    let _ = writeln!(std::io::stderr(), "tachi: logging to {sink_label}");
}

/// Best-effort restrict a file to owner-read/write (0o600) on Unix.
#[cfg(test)]
#[cfg(unix)]
pub(super) fn restrict_file_permissions(path: &std::path::Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(test)]
mod oauth_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::{layer::SubscriberExt, Layer};

    struct Capture(Arc<Mutex<Vec<tracing::Level>>>);
    impl<S: tracing::Subscriber> Layer<S> for Capture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            self.0.lock().unwrap().push(*event.metadata().level());
        }
    }

    #[test]
    fn oauth_debug_payloads_stay_filtered_with_trace_enabled() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new("trace"))
            .with(tracing_subscriber::filter::filter_fn(oauth_log_allowed))
            .with(Capture(events.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(target: "rmcp::transport::auth", "authorization-code-sentinel");
            tracing::trace!(target: "rmcp::transport::auth", "token-sentinel");
            tracing::info!(target: "rmcp::transport::auth", "status");
            tracing::debug!(target: "tachi_server", "ordinary diagnostics");
        });
        assert_eq!(
            *events.lock().unwrap(),
            [tracing::Level::INFO, tracing::Level::DEBUG]
        );
    }
}
