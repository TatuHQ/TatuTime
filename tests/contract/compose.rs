//! Guard for the default `docker compose up -d` stack: the stack must really be
//! able to send mail, and the service that makes that possible must stay a
//! hardened, local-only **capture** inbox.
//!
//! The binary's own default is deliberately untouched — no SMTP host means
//! "not configured", so a host that never opted in cannot send anything. The
//! sending comes from Compose wiring the app service to a Mailpit sidecar; a
//! bare `docker run` has none of it. This guard pins that wiring so it cannot
//! silently regress into "the stack quietly stops sending" or "the inbox
//! becomes a publicly reachable relay".
//!
//! `docker-compose.yml` is plain YAML, but no YAML crate is a dependency and
//! adding one just for this guard would be the wrong trade. The file is read
//! with `include_str!` and split the way Compose sees it: a service is its
//! two-space-indented key plus every following line indented four spaces or
//! more. A structural change that breaks that assumption fails the guard
//! loudly rather than passing blind.

const COMPOSE: &str = include_str!("../../docker-compose.yml");

/// The body of the top-level service `name` (its properties, not the key).
fn service_block(name: &str) -> String {
    let lines: Vec<&str> = COMPOSE.lines().collect();
    let services = lines
        .iter()
        .position(|line| line.trim_end() == "services:")
        .expect("docker-compose.yml must keep a top-level `services:` section");
    let header = format!("  {name}:");
    let start = lines[services + 1..]
        .iter()
        .position(|line| line.trim_end() == header)
        .map(|i| services + 1 + i)
        .unwrap_or_else(|| panic!("docker-compose.yml has no `{name}` service"));
    let mut block = String::new();
    for line in &lines[start + 1..] {
        if line.trim().is_empty() || line.starts_with("    ") {
            block.push_str(line);
            block.push('\n');
        } else {
            break;
        }
    }
    assert!(!block.trim().is_empty(), "the `{name}` service is empty");
    block
}

/// The value of a property at the service's own level (`    key: value`).
fn property<'a>(block: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("    {key}:");
    block.lines().find_map(|line| {
        line.strip_prefix(&prefix)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

/// The value of an `environment:` entry (`      KEY: value`). A Compose
/// environment entry is one indentation level deeper than the service property,
/// and no other section in this file uses that level for `KEY: value`.
fn env_value<'a>(block: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("      {key}:");
    block.lines().find_map(|line| {
        line.strip_prefix(&prefix)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

/// The nested lines of a section property (`    key:` then deeper indentation),
/// trimmed, one per line. Panics when the key is absent.
fn section(block: &str, key: &str) -> String {
    let header = format!("    {key}:");
    let mut out = String::new();
    let mut inside = false;
    for line in block.lines() {
        if !inside {
            if line.trim_end() == header {
                inside = true;
            }
            continue;
        }
        if line.starts_with("      ") {
            out.push_str(line.trim());
            out.push('\n');
        } else if !line.trim().is_empty() {
            break;
        }
    }
    assert!(inside, "`{key}` must be declared on the service");
    out
}

/// The default stack sends through a pinned, hardened Mailpit that is reachable
/// only from the compose network, and the app service is wired to it.
#[test]
fn compose_default_stack_sends_mail_through_a_hardened_local_relay() {
    let app = service_block("tucanotime");
    let relay = service_block("mailpit");

    // --- the relay is a pinned release, never a moving tag -----------------
    let image = property(&relay, "image").expect("mailpit must declare an image");
    let tag = image
        .rsplit_once(':')
        .map(|(_, tag)| tag)
        .unwrap_or_else(|| panic!("mailpit image must carry an explicit tag: {image}"));
    assert_ne!(tag, "latest", "the relay must be pinned: {image}");
    let version = tag.strip_prefix('v').unwrap_or(tag);
    let parts: Vec<&str> = version.split('.').collect();
    assert_eq!(
        parts.len(),
        3,
        "the relay tag must pin an exact release, got `{tag}` in {image}"
    );
    assert!(
        parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
        "the relay tag must pin an exact release, got `{tag}` in {image}"
    );

    // --- hardening matches the app's own bar --------------------------------
    assert_eq!(
        property(&relay, "read_only"),
        Some("true"),
        "the relay root filesystem must be read-only"
    );
    assert!(
        relay.contains("cap_drop:\n      - ALL"),
        "the relay must drop every capability:\n{relay}"
    );
    assert!(
        relay.contains("no-new-privileges:true"),
        "the relay must block privilege escalation:\n{relay}"
    );
    let user = property(&relay, "user")
        .expect("the relay must run as an explicit non-root user")
        .trim_matches('"');
    let uid = user.split(':').next().unwrap_or(user);
    assert!(
        uid != "0" && uid != "root",
        "the relay must run as a non-root user, got `{user}`"
    );
    assert!(
        relay.contains("/tmp"),
        "the relay needs its /tmp tmpfs:\n{relay}"
    );

    // --- the inbox UI is loopback-only, the SMTP port is not published -----
    assert!(
        relay.contains("- \"127.0.0.1:8025:8025\""),
        "the inbox UI must be published on loopback only:\n{relay}"
    );
    assert!(
        !COMPOSE.contains("1025:1025"),
        "the SMTP port must never be published to the host — the app reaches \
         the relay at `mailpit:1025` over the compose network"
    );
    assert!(
        !relay.contains("0.0.0.0:8025"),
        "the inbox UI must not be published on every interface"
    );

    // --- the healthcheck probes the HTTP endpoint ---------------------------
    let health = section(&relay, "healthcheck");
    assert!(
        health.contains("8025") && health.contains("/livez"),
        "the healthcheck must probe the HTTP endpoint:\n{health}"
    );

    // --- the app is wired to the relay, and keeps its own hardening ---------
    assert_eq!(
        env_value(&app, "TUCANO_SMTP_HOST"),
        Some("mailpit"),
        "the app service must resolve the relay by service name"
    );
    assert_eq!(
        env_value(&app, "TUCANO_SMTP_PORT")
            .expect("the app service must set TUCANO_SMTP_PORT")
            .trim_matches('"'),
        "1025",
        "the app service must use the relay's SMTP port"
    );
    let depends = section(&app, "depends_on");
    assert!(
        depends.contains("mailpit:") && depends.contains("condition: service_healthy"),
        "the app must wait for a healthy relay:\n{depends}"
    );
    assert_eq!(
        property(&app, "read_only"),
        Some("true"),
        "the existing app hardening must stay untouched"
    );
    assert!(app.contains("cap_drop:\n      - ALL"));
    assert!(app.contains("no-new-privileges:true"));
    assert_eq!(property(&app, "user"), Some("\"10001:10001\""));
}
