//! TucanoTime — timesheet server: one binary serves the REST API, the web GUI,
//! the machine-readable contract (openapi.json) and its docs UI. Data lives in
//! one folder tree below `TUCANO_DATA_DIR` (no database).
//!
//! The same binary doubles as the operator CLI (#94):
//!   tucano-time                 serve (default)
//!   tucano-time --backup FILE   archive the data dir to FILE (tar.gz + manifest)
//!   tucano-time --restore FILE  unpack a backup into the data dir (--force to overwrite)
//!   tucano-time --health        probe the local server's /healthz (container HEALTHCHECK)
//!   tucano-time --version       print the version

use std::sync::Arc;

use tucano_time::api::AppState;

fn data_dir() -> std::path::PathBuf {
    std::env::var("TUCANO_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("Data"))
}

/// Run one of the operator subcommands. `Ok(true)` means "a CLI action ran,
/// do not serve".
fn run_cli(args: &[String]) -> Result<bool, String> {
    let root = data_dir();
    match args.get(1).map(String::as_str) {
        Some("--version") => {
            println!("tucano-time {}", env!("CARGO_PKG_VERSION"));
            Ok(true)
        }
        Some("--backup") => {
            let Some(out) = args.get(2) else {
                return Err("usage: tucano-time --backup <out.tar.gz>".into());
            };
            let n = tucano_time::backup::create(&root, std::path::Path::new(out))
                .map_err(|e| e.to_string())?;
            println!("backed up {n} file(s) from {} to {out}", root.display());
            Ok(true)
        }
        Some("--restore") => {
            let Some(in_path) = args.get(2) else {
                return Err("usage: tucano-time --restore <in.tar.gz> [--force]".into());
            };
            let force = args.iter().any(|a| a == "--force");
            let notes = tucano_time::backup::restore(std::path::Path::new(in_path), &root, force)
                .map_err(|e| e.to_string())?;
            println!("restored into {}", root.display());
            for n in notes {
                println!("  note: {n}");
            }
            Ok(true)
        }
        Some("--health") => {
            let port = std::env::var("TUCANO_PORT").unwrap_or_else(|_| "8080".into());
            let url = format!("http://127.0.0.1:{port}/healthz");
            let ok = ureq::get(&url)
                .call()
                .map(|r| r.status() == 200u16)
                .unwrap_or(false);
            if ok {
                std::process::exit(0)
            } else {
                std::process::exit(1)
            }
        }
        _ => Ok(false),
    }
}

/// Wait for a termination signal. Resolves with the signal's name so the caller
/// can log it; never panics and never gives up early.
///
/// `docker stop` delivers SIGTERM, an interactive Ctrl-C delivers SIGINT, so
/// the serve path honours whichever arrives first. Only the long-running serve
/// path uses this — the operator subcommands (`--health`, `--backup`,
/// `--restore`) exit before a listener exists and stay unchanged.
#[cfg(unix)]
async fn sigterm() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut stream) => {
            stream.recv().await;
        }
        Err(e) => {
            // Without a handler we must not shut down spuriously: keep waiting
            // (Ctrl-C still works through the other select branch).
            tracing::error!("cannot install SIGTERM handler: {e}");
            std::future::pending::<()>().await;
        }
    }
}

async fn shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        tokio::select! {
            _ = sigterm() => "SIGTERM",
            _ = tokio::signal::ctrl_c() => "SIGINT",
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "SIGINT"
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    if run_cli(&args)? {
        return Ok(());
    }

    let root = data_dir();
    let root_str = root.display().to_string();
    let port = std::env::var("TUCANO_PORT").unwrap_or_else(|_| "8080".to_owned());

    // Open the store first: it creates the data dir that session.key and
    // config.json persistence need.
    let mut store = tucano_time::store::Store::open(&root)?;

    // Single-instance guard (review B6): a second server against the same
    // volume would diverge on cached vault/revocation state and fire every
    // scheduler job twice. Hold an exclusive lock for the process lifetime.
    // The guard lives in the library (`acquire_server_lock`) so it is
    // unit-testable (#262); the returned handle is held until shutdown.
    let server_lock = tucano_time::acquire_server_lock(&root).map_err(|msg| {
        tracing::error!("{msg}");
        msg
    })?;

    // Externalised configuration (#94): env > <data>/config.json > default.
    // A corrupt config file refuses to start rather than silently reverting
    // settings — that is exactly the failure mode this ticket removes.
    let cfg = Arc::new(
        tucano_time::appconfig::AppConfig::load(store.root()).map_err(|e| {
            tracing::error!("{e}");
            e
        })?,
    );

    // Session key: env secret > env *FILE > <data>/session.key (auto-created
    // with fresh randomness on first boot). A restart/update of the container
    // therefore keeps everybody signed in without any operator setup (#94).
    let production = std::env::var("TUCANO_ENV").as_deref() == Ok("production");
    let (secret, _) = tucano_time::auth::resolve_session_secret(
        store.root(),
        std::env::var("TUCANO_SESSION_SECRET").ok(),
        std::env::var("TUCANO_SESSION_SECRET_FILE").ok(),
        production,
    );
    // Cookie Secure is its own axis (review A5): default to on in production,
    // off elsewhere; TUCANO_SECURE_COOKIES=0 permits plain-HTTP LAN serving.
    let secure_cookies = std::env::var("TUCANO_SECURE_COOKIES")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s != "0" && !s.eq_ignore_ascii_case("false"))
        .unwrap_or(production);
    let session = tucano_time::auth::make_session(secret, production, secure_cookies)?;

    // Credential vault: a key is created on first boot; the env key/env key
    // file remain an optional override that always wins. Hard-fail when a store
    // exists but no key can open it (#94 — never start with a silently disabled
    // Settings tab). `vault_enabled` resolves env > config.json > true.
    let (vault_flag, vault_flag_source) = cfg.resolve(
        tucano_time::appconfig::key_def("vault_enabled").expect("vault_enabled is whitelisted"),
        &tucano_time::appconfig::process_env,
    );
    let vault_enabled = matches!(vault_flag, serde_json::Value::Bool(true));
    let mut vault_boot = tucano_time::vault::open_for_boot(
        store.root(),
        std::env::var("TUCANO_SECRET_KEY").ok(),
        std::env::var("TUCANO_SECRET_KEY_FILE").ok(),
        vault_enabled,
    )
    .map_err(|e| {
        tracing::error!("{e}");
        e
    })?;
    // The policy function reports the config reason; when a **valid** env value
    // is what disabled it, say so (it is an operator override). L2: presence
    // alone is not enough — a typo like `TUCANO_VAULT_ENABLED=no` is ignored by
    // resolution, so it must not be reported as an operator override.
    if !vault_enabled && vault_flag_source == tucano_time::appconfig::Source::Env {
        vault_boot.disabled = Some(tucano_time::vault::DisabledReason::Env);
    }
    match (
        vault_boot.source,
        vault_boot.disabled,
        &vault_boot.fingerprint,
    ) {
        (Some(source), None, fingerprint) => tracing::info!(
            source = source.as_str(),
            fingerprint = fingerprint.as_deref().unwrap_or(""),
            "credential vault enabled"
        ),
        (None, Some(reason), _) => {
            tracing::warn!(reason = reason.as_str(), "credential vault disabled")
        }
        _ => tracing::warn!("credential vault state is inconsistent"),
    }

    // Respect a config-file max_docs override (env is handled inside Store::open).
    if std::env::var("TUCANO_MAX_DOCS").is_err() {
        let max = cfg.get_int("max_docs", &tucano_time::appconfig::process_env);
        if max > 0 {
            store = store.with_max_docs(max as usize);
        }
    }
    let state = AppState::boot(store, Arc::new(session), vault_boot, cfg.clone());

    // Background scheduler (#61) with the reminder job (#22). Recurring
    // invoices (#26) and budget alerts (#30) register here too.
    let jobs: Vec<Arc<dyn tucano_time::scheduler::Job>> = vec![
        Arc::new(tucano_time::reminders::ReminderJob::new(
            state.store.clone(),
        )),
        Arc::new(tucano_time::recurring::RecurringJob::new(
            state.store.clone(),
        )),
        Arc::new(tucano_time::budgets::BudgetAlertJob::new(
            state.store.clone(),
        )),
        Arc::new(tucano_time::email_reminders::EmailReminderJob::new(
            state.store.clone(),
            state.email.clone(),
            cfg.get_int("reminder_days", &tucano_time::appconfig::process_env),
            &root,
            // Fallback only: the reminder resolves the saved org profile at
            // send time through the shared `pdf::Org::resolve` (SET-03).
            cfg.get_str("org_name", &tucano_time::appconfig::process_env),
        )),
        Arc::new(tucano_time::accounting::AccountingRetryJob::new(
            state.store.clone(),
            state.accounting.clone(),
        )),
    ];
    let scheduler = Arc::new(tucano_time::scheduler::Scheduler::new(&root, jobs));
    scheduler.spawn(60);

    let app = tucano_time::build_router(state);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port.parse()?)).await?;
    tracing::info!("tucano-time listening on :{port}, data dir {root_str}");
    // Graceful stop (container SIGTERM / Ctrl-C): stop accepting connections,
    // let in-flight requests finish, then fall through to the normal exit so
    // the process ends with status 0 instead of being SIGKILLed mid-write.
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let signal = shutdown_signal().await;
            tracing::info!("shutting down ({signal})");
        })
        .await?;
    tracing::info!("shutdown complete");
    drop(server_lock); // released only when we shut down
    Ok(())
}
