//! Email delivery port (#35). Invoice + reminder emails are rendered by pure
//! functions in this module and handed to an `EmailSender` adapter behind this
//! seam, mirroring `NotificationSender` (#18). The production adapter speaks
//! SMTP with credentials from the vault (#77) / env; tests use an in-memory
//! recorder, so nothing sends email in CI. The trait is synchronous like every
//! other port here; callers on the request path use `spawn_blocking`.
//!
//! Credentials: `smtp.host`, `smtp.port`, `smtp.username`, `smtp.password`,
//! `smtp.from` in the secret vault, or `TUCANO_SMTP_*` env vars. If none are
//! configured the app runs with `DisabledEmailSender` — emails are logged, not
//! sent — degrading safely exactly like the vault itself.

#[derive(Debug, Clone, PartialEq)]
pub struct EmailMessage {
    /// Recipients, in delivery order. Every transport issues one `RCPT TO` per
    /// address and renders them as one comma-joined `To:` header.
    pub to: Vec<String>,
    pub subject: String,
    /// Plain-text body (always present).
    pub text: String,
    /// Optional HTML part.
    pub html: Option<String>,
    /// Attachments in send order: the invoice PDF first, then any expense
    /// receipts the admin chose to include.
    pub attachments: Vec<(String, Vec<u8>)>,
    /// Sender display name override (#146); None keeps the built-in
    /// "TucanoTime" — resolved from the org profile by the API layer.
    pub from_name: Option<String>,
    /// Reply-To header (#146): honoured by every transport; None omits it.
    pub reply_to: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EmailError {
    #[error("email transport failed: {0}")]
    Transport(String),
    #[error("recipient address is invalid")]
    Recipient,
    /// A header-bound value (or the recipient) carried a control character
    /// (#S1). Refused outright rather than stripped: a mangled address a caller
    /// could mistake for the real one is worse than a refusal. The field is
    /// named in the message; the offending value never is.
    #[error("message {0} contains a control character")]
    Header(&'static str),
}

/// Delivers an `EmailMessage`. Adapters must never log message bodies or
/// credentials. `transport()` names the delivery path so API responses and
/// the GUI can be honest about whether a message actually left the building
/// (#130: with no SMTP configured `send` succeeds silently, and "sent" in
/// the UI was a lie).
pub trait EmailSender: Send + Sync {
    fn send(&self, msg: &EmailMessage) -> Result<(), EmailError>;
    /// Stable machine-readable transport id: `"smtp"`, `"disabled"`, or
    /// `"recording"` (test adapter). Serialised into email endpoint results.
    fn transport(&self) -> &'static str;
}

/// Sends nothing; logs at info. The default when SMTP is not configured.
#[derive(Debug, Default)]
pub struct DisabledEmailSender;

impl EmailSender for DisabledEmailSender {
    fn send(&self, msg: &EmailMessage) -> Result<(), EmailError> {
        // Refused on this transport too: the guarded values are the ones it
        // logs, and a folded one must not forge a log line either.
        guard_headers(msg)?;
        tracing::info!(to = %msg.to.join(", "), subject = %msg.subject, "email (SMTP not configured, not sent)");
        Ok(())
    }
    fn transport(&self) -> &'static str {
        "disabled"
    }
}

/// Test/CI adapter: records messages in memory, validating the recipients.
#[derive(Debug, Default)]
pub struct RecordingEmailSender {
    sent: std::sync::Mutex<Vec<EmailMessage>>,
}

impl RecordingEmailSender {
    pub fn messages(&self) -> Vec<EmailMessage> {
        self.sent.lock().unwrap().clone()
    }
}

impl EmailSender for RecordingEmailSender {
    fn send(&self, msg: &EmailMessage) -> Result<(), EmailError> {
        guard_headers(msg)?;
        if !recipients_are_addresses(&msg.to) {
            return Err(EmailError::Recipient);
        }
        self.sent.lock().unwrap().push(msg.clone());
        Ok(())
    }
    fn transport(&self) -> &'static str {
        "recording"
    }
}

/// True when `value` carries a control character: any C0 (U+0000-U+001F,
/// CR, LF and NUL included), DEL or C1. None of them belongs in an address, a
/// subject or a display name, and CR/LF are exactly what folds a header or
/// injects a line into the SMTP dialogue (#S1).
pub fn has_control_characters(value: &str) -> bool {
    value.chars().any(char::is_control)
}

/// The sink guard (#S1): refuse a message whose header-bound values -- or its
/// recipient -- carry a control character, before any of them can reach a
/// header line (`From`/`To`/`Subject`/`Reply-To`), the attachment filename, or
/// the `RCPT TO:<...>` command. Called by `build_mime` *and* by every
/// transport, so a value that arrives by any route -- a resolved subject that
/// never went through `valid_address`, say -- is still refused. Fail-closed and
/// explicit: a hostile value is never silently stripped and shipped mangled.
pub fn guard_headers(msg: &EmailMessage) -> Result<(), EmailError> {
    // Every recipient is header-bound: one hostile entry would fold the `To:`
    // line or the SMTP dialogue, so the whole message is refused.
    if msg.to.iter().any(|addr| has_control_characters(addr)) {
        return Err(EmailError::Header("recipient"));
    }
    let fields = [
        ("subject", Some(msg.subject.as_str())),
        ("reply-to", msg.reply_to.as_deref()),
        ("sender name", msg.from_name.as_deref()),
    ];
    for (field, value) in fields {
        if value.is_some_and(has_control_characters) {
            return Err(EmailError::Header(field));
        }
    }
    if msg
        .attachments
        .iter()
        .any(|(name, _)| has_control_characters(name))
    {
        return Err(EmailError::Header("attachment filename"));
    }
    Ok(())
}

/// True when the list is non-empty and every entry is an address this port
/// will hand to a relay. Empty lists are refused: a message with no `RCPT TO`
/// is never a successful send.
pub fn recipients_are_addresses(to: &[String]) -> bool {
    !to.is_empty() && to.iter().all(|addr| valid_address(addr))
}

/// A deliberately small, conservative address check (not full RFC 5322):
/// one `@`, no spaces, non-empty local, dotted domain. Control characters are
/// refused on the raw value, before `trim()` could hide a trailing one.
pub fn valid_address(addr: &str) -> bool {
    if has_control_characters(addr) {
        return false;
    }
    let addr = addr.trim();
    if addr.is_empty() || addr.contains(' ') {
        return false;
    }
    let mut parts = addr.split('@');
    let local = parts.next().unwrap_or_default();
    let domain = match parts.next() {
        Some(d) => d,
        None => return false, // no '@'
    };
    if parts.next().is_some() {
        return false; // more than one '@'
    }
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains("..")
}

/// The hard cap on recipients for one invoice email. A typo that turns a
/// distribution list into hundreds of RCPT commands is refused before any
/// message is assembled.
pub const MAX_RECIPIENTS: usize = 10;

/// The RFC 5321 maximum address length (the path including the local part).
pub const MAX_RECIPIENT_CHARS: usize = 320;

/// Why one recipient entry is unusable, or `None` when it is fine.
///
/// Deliberately narrower than [`valid_address`]: the API rule is "one `@`, no
/// control characters, trimmed, at most 320 chars", so a bare `a@b` is allowed
/// here even though the transport's stricter check would refuse it. Control
/// characters are tested on the raw value, before `trim()` could hide a
/// trailing CR/LF (the header-injection guard).
pub fn recipient_problem(value: &str) -> Option<&'static str> {
    if has_control_characters(value) {
        return Some("recipient contains a control character");
    }
    let address = value.trim();
    if address.is_empty() {
        return Some("recipient is empty");
    }
    if address.chars().count() > MAX_RECIPIENT_CHARS {
        return Some("recipient is longer than 320 characters");
    }
    if address.matches('@').count() != 1 {
        return Some("recipient must contain exactly one '@'");
    }
    None
}

/// Resolve a request's recipient list (`None`/empty = the customer default).
///
/// Validation happens for **every** entry before anything is sent, so a bad
/// address can never ride along behind a good one. `default` is the customer's
/// billing address: when the caller supplies nothing and it is empty, the
/// existing 422 (`customer has no billing email on file`) is preserved.
pub fn resolve_recipients(
    raw: &[String],
    default: Option<&str>,
) -> Result<Vec<String>, Vec<(String, String)>> {
    if raw.is_empty() {
        return match default.map(str::trim).filter(|addr| !addr.is_empty()) {
            Some(addr) => Ok(vec![addr.to_string()]),
            None => Err(vec![(
                "email".into(),
                "customer has no billing email on file".into(),
            )]),
        };
    }
    if raw.len() > MAX_RECIPIENTS {
        return Err(vec![(
            "recipients".into(),
            format!("at most {MAX_RECIPIENTS} recipients are allowed"),
        )]);
    }
    let mut errors = Vec::new();
    let mut recipients: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (index, entry) in raw.iter().enumerate() {
        match recipient_problem(entry) {
            Some(problem) => errors.push((format!("recipients[{index}]"), problem.to_string())),
            None => {
                let address = entry.trim().to_string();
                // Case-insensitive de-duplication, first occurrence (with its
                // original casing) wins and the given order is preserved.
                let key = address.to_ascii_lowercase();
                if !seen.contains(&key) {
                    seen.push(key);
                    recipients.push(address);
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(recipients)
    } else {
        Err(errors)
    }
}

// ------------------------------------------------------------ SMTP adapter --

/// SMTP connection settings. `username` + `password`, when both present,
/// enable authentication. Port 465 uses verified implicit TLS, 587 requires
/// STARTTLS; other ports retain the trusted-local-relay plaintext mode.
#[derive(Debug, Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub from: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl SmtpConfig {
    /// Builds a config from vault keys (`smtp.*`) falling back to
    /// `TUCANO_SMTP_*` env vars. `None` when no host is configured.
    pub fn from_sources(vault: Option<&crate::vault::SecretVault>) -> Option<Self> {
        let get = |key: &str, env: &str| -> Option<String> {
            vault
                .and_then(|v| v.get(key))
                .or_else(|| std::env::var(env).ok())
        };
        let host = get("smtp.host", "TUCANO_SMTP_HOST")?;
        let port = get("smtp.port", "TUCANO_SMTP_PORT")
            .and_then(|p| p.parse().ok())
            .unwrap_or(25);
        let from =
            get("smtp.from", "TUCANO_SMTP_FROM").unwrap_or_else(|| "noreply@localhost".into());
        Some(Self {
            host,
            port,
            from,
            username: get("smtp.username", "TUCANO_SMTP_USERNAME"),
            password: get("smtp.password", "TUCANO_SMTP_PASSWORD"),
        })
    }
}

/// Production `EmailSender`: SMTP over a blocking TCP connection.
#[derive(Debug)]
pub struct SmtpEmailSender {
    config: SmtpConfig,
}

impl SmtpEmailSender {
    pub fn new(config: SmtpConfig) -> Self {
        Self { config }
    }

    fn send_encrypted(&self, msg: &EmailMessage, implicit_tls: bool) -> Result<(), EmailError> {
        use lettre::Transport;
        use lettre::address::Envelope;
        use lettre::transport::smtp::authentication::Credentials;

        let mut transport = if implicit_tls {
            lettre::SmtpTransport::relay(&self.config.host)
        } else {
            lettre::SmtpTransport::starttls_relay(&self.config.host)
        }
        .map_err(|_| {
            EmailError::Transport(
                "could not configure verified SMTP TLS; check the server hostname".into(),
            )
        })?
        .port(self.config.port)
        .timeout(Some(std::time::Duration::from_secs(15)));
        match (
            self.config
                .username
                .as_deref()
                .filter(|value| !value.is_empty()),
            self.config
                .password
                .as_deref()
                .filter(|value| !value.is_empty()),
        ) {
            (Some(username), Some(password)) => {
                transport = transport
                    .credentials(Credentials::new(username.to_string(), password.to_string()));
            }
            (None, None) => {}
            _ => {
                return Err(EmailError::Transport(
                    "SMTP authentication requires both username and password".into(),
                ));
            }
        }
        let from = self
            .config
            .from
            .parse::<lettre::Address>()
            .map_err(|_| EmailError::Transport("SMTP From address is invalid".into()))?;
        let recipients = msg
            .to
            .iter()
            .map(|address| address.parse::<lettre::Address>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| EmailError::Recipient)?;
        let envelope = Envelope::new(Some(from), recipients).map_err(|_| EmailError::Recipient)?;
        let wire = build_mime_with_sender(msg, Some(&self.config.from))?;
        transport.build().send_raw(&envelope, wire.as_bytes())
            .map(|_| ())
            .map_err(|error| {
                let message = if error.is_timeout() {
                    "SMTP connection timed out; check the host, port and firewall"
                } else if error.is_tls() || error.is_client() {
                    "SMTP TLS negotiation failed; check the hostname, certificate and port (465 TLS / 587 STARTTLS)"
                } else if error.status().is_some_and(|status| status.to_string().starts_with("535")) {
                    "SMTP authentication was rejected; check the username, password or provider app-password policy"
                } else if error.is_permanent() || error.is_transient() || error.is_response() {
                    "SMTP server rejected the message; check the sender address, recipient and provider sending policy"
                } else {
                    "SMTP connection failed; check the server hostname, port and network access"
                };
                EmailError::Transport(message.into())
            })
    }
}

/// Resolve the live transport from the vault + `TUCANO_SMTP_*` env.
/// `DisabledEmailSender` when no host is configured.
pub fn sender_from_sources(
    vault: Option<&crate::vault::SecretVault>,
) -> std::sync::Arc<dyn EmailSender> {
    match SmtpConfig::from_sources(vault) {
        Some(config) => std::sync::Arc::new(SmtpEmailSender::new(config)),
        None => std::sync::Arc::new(DisabledEmailSender),
    }
}

/// The production transport installed by `AppState::boot`.
///
/// It holds the live vault **slot**, not a boot-time snapshot, and re-resolves
/// `smtp.*` on every `send`/`transport` call. That is what makes
/// `PUT /api/admin/email` — and an admin-API enable/regenerate — take effect on
/// the very next message without a restart. Tests replace the whole transport
/// through `AppState::with_email`, so the injection seam is unchanged.
pub struct DynamicEmailSender {
    vault: std::sync::Arc<crate::vault::VaultSlot>,
}

impl DynamicEmailSender {
    pub fn new(vault: std::sync::Arc<crate::vault::VaultSlot>) -> Self {
        Self { vault }
    }

    fn resolve(&self) -> std::sync::Arc<dyn EmailSender> {
        let vault = self.vault.get();
        sender_from_sources(vault.as_deref())
    }
}

impl EmailSender for DynamicEmailSender {
    fn send(&self, msg: &EmailMessage) -> Result<(), EmailError> {
        self.resolve().send(msg)
    }
    fn transport(&self) -> &'static str {
        self.resolve().transport()
    }
}

impl EmailSender for SmtpEmailSender {
    fn send(&self, msg: &EmailMessage) -> Result<(), EmailError> {
        guard_headers(msg)?;
        if !recipients_are_addresses(&msg.to) {
            return Err(EmailError::Recipient);
        }
        if self.config.port == 465 || self.config.port == 587 {
            return self.send_encrypted(msg, self.config.port == 465);
        }
        let stream = std::net::TcpStream::connect((self.config.host.as_str(), self.config.port))
            .map_err(|e| EmailError::Transport(e.to_string()))?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(15)))
            .ok();
        SmtpConn { stream }.run(&self.config, msg)
    }
    fn transport(&self) -> &'static str {
        "smtp"
    }
}

struct SmtpConn {
    stream: std::net::TcpStream,
}

impl SmtpConn {
    /// Reads SMTP replies until the final `NNN ` line; returns its code.
    fn read_reply(&mut self) -> Result<u16, EmailError> {
        use std::io::Read;
        let mut buf: Vec<u8> = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = self
                .stream
                .read(&mut byte)
                .map_err(|e| EmailError::Transport(e.to_string()))?;
            if n == 0 {
                return Err(EmailError::Transport("connection closed".into()));
            }
            buf.push(byte[0]);
            // A reply is done at a CRLF that closes a line starting "NNN ".
            if buf.len() >= 5 && buf.ends_with(b"\r\n") {
                let lines: Vec<&[u8]> = buf
                    .split(|b| *b == b'\n')
                    .filter(|l| !l.is_empty())
                    .collect();
                if lines
                    .iter()
                    .all(|l| l.len() >= 4 && l[..3].iter().all(|c| c.is_ascii_digit()))
                    && let Some(last) = lines.last()
                    && last[3] == b' '
                {
                    break;
                }
            }
        }
        let code = std::str::from_utf8(&buf[..3])
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| EmailError::Transport("malformed reply".into()))?;
        Ok(code)
    }

    /// Send one SMTP command line and check the reply code.
    ///
    /// M2: an `AUTH` command must never appear in the error — the payload is
    /// the credential. The base64 lines that follow it are handled by
    /// [`Self::command_labeled`] with an explicit redacted label.
    fn command(&mut self, line: &str, expect: &[u16]) -> Result<(), EmailError> {
        let label = if line
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("AUTH"))
        {
            "AUTH <redacted>"
        } else {
            line
        };
        self.command_labeled(line, label, expect)
    }

    /// Like [`Self::command`] with a caller-chosen diagnostic label. Used for
    /// the base64 username/password argument lines of `AUTH LOGIN`, which are
    /// secrets but not recognisable by their shape.
    fn command_labeled(
        &mut self,
        line: &str,
        label: &str,
        expect: &[u16],
    ) -> Result<(), EmailError> {
        use std::io::Write;
        self.stream
            .write_all(line.as_bytes())
            .and_then(|_| self.stream.write_all(b"\r\n"))
            .and_then(|_| self.stream.flush())
            .map_err(|e| EmailError::Transport(e.to_string()))?;
        let code = self.read_reply()?;
        if expect.contains(&code) {
            Ok(())
        } else {
            Err(unexpected_reply(code, label))
        }
    }

    fn run(&mut self, cfg: &SmtpConfig, msg: &EmailMessage) -> Result<(), EmailError> {
        self.read_reply()?; // 220 greeting
        self.command("EHLO tucanotime", &[250])?;
        if let (Some(u), Some(p)) = (&cfg.username, &cfg.password) {
            self.command("AUTH LOGIN", &[334])?;
            // M2: the base64 username/password lines are credentials — label
            // them generically so a 535 never echoes the secret.
            self.command_labeled(&auth_line(u), "AUTH <redacted>", &[334])?;
            self.command_labeled(&auth_line(p), "AUTH <redacted>", &[235])?;
        }
        self.command(&format!("MAIL FROM:<{}>", cfg.from), &[250])?;
        // One envelope recipient per address: all of them see the same DATA.
        for recipient in &msg.to {
            self.command(&format!("RCPT TO:<{recipient}>"), &[250, 251])?;
        }
        self.command("DATA", &[354])?;
        self.write_message(msg, &cfg.from)?;
        self.command("QUIT", &[221])?;
        Ok(())
    }

    fn write_message(&mut self, msg: &EmailMessage, from: &str) -> Result<(), EmailError> {
        use std::io::Write;
        let body = build_mime_with_sender(msg, Some(from))?;
        let stuffed: String = body
            .lines()
            .map(|l| {
                if l.starts_with('.') {
                    format!(".{l}")
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\r\n");
        self.stream
            .write_all(stuffed.as_bytes())
            .and_then(|_| self.stream.write_all(b"\r\n.\r\n"))
            .and_then(|_| self.stream.flush())
            .map_err(|e| EmailError::Transport(e.to_string()))?;
        let code = self.read_reply()?;
        if code == 250 {
            Ok(())
        } else {
            Err(EmailError::Transport(format!("data rejected: {code}")))
        }
    }
}

fn auth_line(s: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(s)
}

/// A refusal that names the command it answered. `label` is the redacted form
/// for credential-bearing commands (M2), so the base64 `AUTH LOGIN` argument
/// never reaches an error message, a log line or an API body.
fn unexpected_reply(code: u16, label: &str) -> EmailError {
    EmailError::Transport(format!("unexpected reply {code} to {label}"))
}

/// RFC 2047: headers stay ASCII unless they carry a display name or a
/// non-ASCII subject (org/customer names — #112 puts the customer name in the
/// copy subject, so "Café München" must not emit raw UTF-8 header bytes that
/// strict relays reject). Encoded words are Base64 (`?B?`) — deterministic
/// and safe for any value length.
pub fn encode_header(value: &str) -> String {
    if value.is_ascii() {
        return value.to_string();
    }
    use base64::Engine;
    let enc = base64::engine::general_purpose::STANDARD.encode(value.as_bytes());
    format!("=?UTF-8?B?{enc}?=")
}

/// Serialises headers + body, multipart when an attachment is present.
/// Refuses, before a single header is written, a message whose header-bound
/// values carry a control character (`guard_headers`) -- the choke point every
/// caller shares.
pub fn build_mime(msg: &EmailMessage) -> Result<String, EmailError> {
    build_mime_with_sender(msg, None)
}

fn build_mime_with_sender(msg: &EmailMessage, sender: Option<&str>) -> Result<String, EmailError> {
    guard_headers(msg)?;
    if sender.is_some_and(|address| {
        recipient_problem(address).is_some()
            || address.chars().any(char::is_whitespace)
            || address.contains(['<', '>'])
    }) {
        return Err(EmailError::Transport("SMTP From address is invalid".into()));
    }
    let from_display = msg
        .from_name
        .as_deref()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or("TucanoTime");
    let from_header = match sender {
        Some(address) => format!("{} <{address}>", encode_header(from_display)),
        None => encode_header(from_display),
    };
    let mut h = format!(
        "From: {}\r\nTo: {}\r\nSubject: {}\r\nDate: {}\r\n",
        from_header,
        encode_header(&msg.to.join(", ")),
        encode_header(&msg.subject),
        chrono::Utc::now().to_rfc2822(),
    );
    if let Some(reply) = msg.reply_to.as_deref().filter(|r| !r.trim().is_empty()) {
        h.push_str(&format!("Reply-To: {}\r\n", encode_header(reply.trim())));
    }
    match (&msg.html, msg.attachments.is_empty()) {
        (None, true) => {
            h.push_str(
                "MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
            );
            h.push_str(&msg.text);
        }
        (Some(html), true) => {
            h.push_str("MIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n");
            h.push_str(html);
        }
        _ => {
            h.push_str(
                "MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"tt\"\r\n\r\n",
            );
            h.push_str(
                "--tt\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
            );
            h.push_str(&msg.text);
            if let Some(html) = &msg.html {
                h.push_str("\r\n--tt\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n");
                h.push_str(html);
            }
            for (name, bytes) in &msg.attachments {
                use base64::Engine;
                let enc = base64::engine::general_purpose::STANDARD.encode(bytes);
                let ctype = attachment_content_type(name);
                h.push_str(&format!(
                    "\r\n--tt\r\nContent-Type: {ctype}\r\nContent-Disposition: attachment; filename=\"{name}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n"
                ));
                for chunk in enc.as_bytes().chunks(76) {
                    h.push_str(&String::from_utf8_lossy(chunk));
                    h.push_str("\r\n");
                }
            }
            h.push_str("--tt--");
        }
    }
    Ok(h)
}

/// The MIME type for an attachment, from the only hint storage keeps: the
/// filename extension. Unknown types are opaque and download safely.
fn attachment_content_type(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "tif" | "tiff" => "image/tiff",
        "heic" => "image/heic",
        _ => "application/octet-stream",
    }
}

// ------------------------------------------------------------- templates ---

/// Subject for an invoice email.
pub fn invoice_subject(number: &str, org: &str) -> String {
    format!("Invoice {number} from {org}")
}

/// Plain-text invoice cover email. `amount` is pre-formatted (e.g.
/// "1,200.00 EUR") so this stays locale-agnostic. `pdf_attached` tells the
/// customer the document rides along (#113). `payment` is the customer's
/// configured payment-details block — when set, the email carries a
/// "How to pay" section (no hosted checkout exists by design: the client
/// pays in on the bank, and settlement is recorded manually).
#[allow(clippy::too_many_arguments)]
pub fn render_invoice_email(
    customer: &str,
    number: &str,
    amount: &str,
    due: Option<&str>,
    org: &str,
    pdf_attached: bool,
    payment: Option<&str>,
) -> String {
    let mut s = format!("Hi {customer},\n\nPlease find invoice {number} for {amount}");
    if let Some(due) = due {
        s.push_str(&format!(", due {due}"));
    }
    s.push('.');
    if pdf_attached {
        s.push_str(" The PDF document is attached.");
    }
    if let Some(block) = payment.map(str::trim).filter(|b| !b.is_empty()) {
        s.push_str(&format!("\n\nHow to pay:\n{block}"));
    }
    s.push_str(&format!("\n\nThank you,\n{org}\n"));
    s
}

/// Plain-text cover note for a PDF copy sent to a third party — e.g. the
/// company accountant (#112). `requester` is the authenticated user who asked
/// for the copy, named so the recipient knows who to bill it back to.
pub fn render_copy_email(
    number: &str,
    customer: &str,
    requester: &str,
    note: Option<&str>,
    org: &str,
) -> String {
    let mut s = format!(
        "A copy of invoice {number} for {customer} is attached as a PDF, sent at the request \
         of {requester}."
    );
    if let Some(note) = note.map(str::trim).filter(|n| !n.is_empty()) {
        s.push_str(&format!("\n\nNote: {note}"));
    }
    s.push_str(&format!("\n\n{org}\n"));
    s
}

/// Plain-text overdue-payment reminder for a single invoice. `pdf_attached`
/// names the archived document riding along (#113). `payment` re-sends the
/// customer's payment details so an overdue client has the account handy.
#[allow(clippy::too_many_arguments)]
pub fn render_reminder_email(
    customer: &str,
    number: &str,
    amount: &str,
    due: &str,
    days_over: i64,
    org: &str,
    pdf_attached: bool,
    payment: Option<&str>,
) -> String {
    let document = if pdf_attached {
        " A PDF copy of the invoice is attached."
    } else {
        ""
    };
    let mut s = format!(
        "Hi {customer},\n\nThis is a reminder that invoice {number} for {amount} was due {due} \
         and is now {days_over} day(s) overdue.{document}\n\nIf you have already paid, please \
         disregard this notice."
    );
    if let Some(block) = payment.map(str::trim).filter(|b| !b.is_empty()) {
        s.push_str(&format!("\n\nHow to pay:\n{block}"));
    }
    s.push_str(&format!("\n\nThank you,\n{org}\n"));
    s
}

/// Should an overdue invoice get a reminder now, given a cadence in days since
/// the last reminder? Pure so the schedule is testable with a `FixedClock`.
pub fn due_for_reminder(
    last_reminder: Option<chrono::NaiveDate>,
    today: chrono::NaiveDate,
    cadence_days: i64,
) -> bool {
    match last_reminder {
        None => true,
        Some(last) => (today - last).num_days() >= cadence_days,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn address_validation() {
        assert!(valid_address("a@b.co"));
        assert!(valid_address("  a@b.co  "));
        assert!(!valid_address("no-at"));
        assert!(!valid_address("two@@b.co"));
        assert!(!valid_address("a b@c.co"));
        assert!(!valid_address("a@b"));
        assert!(!valid_address("a@b."));
        assert!(!valid_address(""));
        // #S1: control characters are rejected on the raw value, before trim.
        assert!(!valid_address(
            "inv@corp.example\r\nBcc:attacker@evil.example"
        ));
        assert!(!valid_address("inv@corp.example\n"));
        assert!(!valid_address("inv@corp.example\0"));
        assert!(!valid_address("inv\tcorp@example.co"));
    }

    #[test]
    fn recording_sender_rejects_bad_recipient() {
        let s = RecordingEmailSender::default();
        let ok = s.send(&EmailMessage {
            to: vec!["x@y.co".into()],
            subject: "s".into(),
            text: "t".into(),
            html: None,
            attachments: vec![],
            from_name: None,
            reply_to: None,
        });
        assert!(ok.is_ok());
        let bad = s.send(&EmailMessage {
            to: vec!["nope".into()],
            subject: "s".into(),
            text: "t".into(),
            html: None,
            attachments: vec![],
            from_name: None,
            reply_to: None,
        });
        assert!(matches!(bad, Err(EmailError::Recipient)));
        assert_eq!(s.messages().len(), 1);
    }

    #[test]
    fn copy_email_names_requester_and_carries_note() {
        let s = render_copy_email(
            "INV-9",
            "Tucano Time",
            "Pat Admin",
            Some("for the books"),
            "Tucano",
        );
        assert!(s.contains("INV-9") && s.contains("Tucano Time") && s.contains("Pat Admin"));
        assert!(s.contains("for the books"));
        // Blank notes drop the section entirely.
        let bare = render_copy_email("INV-9", "Tucano Time", "Pat", Some("   "), "Tucano");
        assert!(!bare.contains("Note:"));
    }

    #[test]
    fn templates_include_key_fields() {
        let inv = render_invoice_email(
            "Tucano Time",
            "INV-1",
            "1,200.00 EUR",
            Some("2026-10-19"),
            "Tucano",
            true,
            None,
        );
        assert!(
            inv.contains("INV-1") && inv.contains("1,200.00 EUR") && inv.contains("2026-10-19")
        );
        // No payment details configured: no "How to pay" section invented.
        assert!(!inv.contains("How to pay"));
        // Configured: the block rides along verbatim.
        let with_pay = render_invoice_email(
            "Tucano Time",
            "INV-1",
            "1,200.00 EUR",
            Some("2026-10-19"),
            "Tucano",
            true,
            Some("IBAN: EX00 0000 0000 0000 0000 00"),
        );
        assert!(with_pay.contains("How to pay:") && with_pay.contains("IBAN: EX00"));
        let rem = render_reminder_email(
            "Tucano Time",
            "INV-1",
            "1,200.00 EUR",
            "2026-10-19",
            12,
            "Tucano",
            true,
            None,
        );
        assert!(
            rem.contains("12 day(s) overdue") && rem.contains("INV-1") && rem.contains("PDF copy")
        );
    }

    #[test]
    fn reminder_cadence_gate() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        assert!(due_for_reminder(None, today, 7));
        let last = NaiveDate::from_ymd_opt(2026, 10, 2).unwrap();
        assert!(!due_for_reminder(Some(last), today, 7));
        let last = NaiveDate::from_ymd_opt(2026, 9, 28).unwrap();
        assert!(due_for_reminder(Some(last), today, 7));
    }

    #[test]
    fn mime_builds_multipart_with_attachment() {
        let m = EmailMessage {
            to: vec!["x@y.co".into()],
            subject: "S".into(),
            text: "body".into(),
            html: None,
            attachments: vec![("inv.csv".into(), b"hi".to_vec())],
            from_name: None,
            reply_to: None,
        };
        let mime = build_mime(&m).expect("a legitimate message builds");
        assert!(mime.contains("multipart/mixed"));
        assert!(mime.contains("attachment; filename=\"inv.csv\""));
        assert!(mime.contains("Content-Type: application/octet-stream"));
        assert!(mime.contains("aGk")); // base64("hi")
    }

    #[test]
    fn non_ascii_headers_are_rfc2047_encoded() {
        assert_eq!(encode_header("Plain Subject"), "Plain Subject");
        let enc = encode_header("Copy of invoice INV-1 for Café München");
        assert!(
            enc.starts_with("=?UTF-8?B?") && enc.ends_with("?="),
            "{enc}"
        );
        // The raw bytes must not survive into the header block.
        let m = EmailMessage {
            to: vec!["x@y.co".into()],
            subject: "Café München".into(),
            text: "body".into(),
            html: None,
            attachments: vec![],
            from_name: None,
            reply_to: None,
        };
        let mime = build_mime(&m).expect("a legitimate message builds");
        assert!(mime.contains("Subject: =?UTF-8?B?"), "{mime}");
        assert!(!mime.contains("Café"), "no raw UTF-8 in headers");
        // Bodies keep their UTF-8, declared 8bit.
        assert!(mime.contains("Content-Transfer-Encoding: 8bit"));
    }

    #[test]
    fn sender_name_and_reply_to_headers() {
        // Default: built-in From, no Reply-To — unchanged for legacy users.
        let m = EmailMessage {
            to: vec!["x@y.co".into()],
            subject: "S".into(),
            text: "b".into(),
            html: None,
            attachments: vec![],
            from_name: None,
            reply_to: None,
        };
        let mime = build_mime(&m).expect("a legitimate message builds");
        assert!(mime.contains("From: TucanoTime\r\n"), "{mime}");
        assert!(!mime.contains("Reply-To"), "{mime}");
        // Configured (#146): display name RFC 2047 when non-ASCII + Reply-To.
        let m2 = EmailMessage {
            from_name: Some("Café Tucano".into()),
            reply_to: Some("billing@tucano.test".into()),
            ..m
        };
        let mime2 = build_mime(&m2).expect("a legitimate message builds");
        assert!(
            mime2.starts_with("From: =?UTF-8?B?") || mime2.contains("From: =?UTF-8?B?"),
            "{mime2}"
        );
        assert!(
            mime2.contains("Reply-To: billing@tucano.test\r\n"),
            "{mime2}"
        );
        assert!(!mime2.contains("Café"), "no raw UTF-8 in headers");
    }

    #[test]
    fn transports_self_report() {
        assert_eq!(DisabledEmailSender.transport(), "disabled");
        assert_eq!(RecordingEmailSender::default().transport(), "recording");
        assert_eq!(
            SmtpEmailSender::new(SmtpConfig {
                host: "h".into(),
                port: 25,
                from: "f@x.co".into(),
                username: None,
                password: None,
            })
            .transport(),
            "smtp"
        );
    }

    #[test]
    fn pdf_attachments_are_labelled_as_pdf() {
        let m = EmailMessage {
            to: vec!["x@y.co".into()],
            subject: "S".into(),
            text: "body".into(),
            html: None,
            attachments: vec![("INV-0001.PDF".into(), b"%PDF-1.4".to_vec())],
            from_name: None,
            reply_to: None,
        };
        let mime = build_mime(&m).expect("a legitimate message builds");
        assert!(mime.contains("Content-Type: application/pdf"), "{mime}");
    }

    #[test]
    fn legitimate_display_names_and_recipients_still_build() {
        // The guard rejects injections, not address shapes: a display-name
        // recipient and an ASCII display name build exactly as they always did.
        let m = EmailMessage {
            to: vec!["billing@tucanotime.test".into()],
            subject: "Invoice INV-0001 from Tucano".into(),
            text: "b".into(),
            html: None,
            attachments: vec![],
            from_name: Some("Tucano Time".into()),
            reply_to: Some("billing@tucano.test".into()),
        };
        let mime = build_mime(&m).expect("a legitimate message builds");
        assert!(mime.contains("From: Tucano Time\r\n"), "{mime}");
        assert!(mime.contains("To: billing@tucanotime.test\r\n"), "{mime}");
        assert!(mime.contains("Reply-To: billing@tucano.test\r\n"), "{mime}");
        let named = EmailMessage {
            to: vec!["Tucano Time <billing@tucanotime.example>".into()],
            ..m.clone()
        };
        let mime = build_mime(&named).expect("a display-name recipient builds");
        assert!(
            mime.contains("To: Tucano Time <billing@tucanotime.example>\r\n"),
            "{mime}"
        );
        // The recording transport takes it exactly as before.
        let rec = RecordingEmailSender::default();
        assert!(rec.send(&m).is_ok());
        assert_eq!(rec.messages().len(), 1);
    }

    #[test]
    fn control_characters_are_refused_at_the_sink() {
        // Every header-bound value the builder writes is checked, including the
        // ones that never pass through `valid_address` (a resolved subject, a
        // display name). A refused message is refused outright -- never
        // stripped-and-shipped -- and the value never reaches a header.
        let base = EmailMessage {
            to: vec!["ok@x.co".into()],
            subject: "S".into(),
            text: "b".into(),
            html: None,
            attachments: vec![],
            from_name: None,
            reply_to: None,
        };
        for hostile in [
            "inv@corp.example\r\nBcc: attacker@evil.example",
            "inv@corp.example\nBcc: attacker@evil.example",
            "inv@corp.example\0",
        ] {
            let m = EmailMessage {
                to: vec![hostile.into()],
                ..base.clone()
            };
            assert!(matches!(
                build_mime(&m),
                Err(EmailError::Header("recipient"))
            ));
            assert!(!valid_address(hostile));
        }
        for hostile in ["S\r\nBcc: attacker@evil.example", "S\nX", "S\0"] {
            let m = EmailMessage {
                subject: hostile.into(),
                ..base.clone()
            };
            assert!(matches!(build_mime(&m), Err(EmailError::Header("subject"))));
        }
        let m = EmailMessage {
            reply_to: Some("billing@x.co\r\nBcc: attacker@evil.example".into()),
            ..base.clone()
        };
        assert!(matches!(
            build_mime(&m),
            Err(EmailError::Header("reply-to"))
        ));
        let m = EmailMessage {
            from_name: Some("Tucano\r\nBcc: attacker@evil.example".into()),
            ..base.clone()
        };
        assert!(matches!(
            build_mime(&m),
            Err(EmailError::Header("sender name"))
        ));
        let m = EmailMessage {
            attachments: vec![("inv\r\nBcc: x.pdf".into(), b"%PDF-1.4".to_vec())],
            ..base.clone()
        };
        assert!(matches!(
            build_mime(&m),
            Err(EmailError::Header("attachment filename"))
        ));
        // The refusal names the field and never leaks the offending value.
        let err = build_mime(&EmailMessage {
            subject: "S\r\nBcc: attacker@evil.example".into(),
            ..base
        })
        .unwrap_err();
        let text = err.to_string();
        assert!(!text.contains("attacker@evil.example"), "{text}");
        assert!(!text.contains('\r') && !text.contains('\n'), "{text}");
    }

    #[test]
    fn transports_refuse_control_characters_without_recording() {
        let m = EmailMessage {
            to: vec!["ok@x.co".into()],
            subject: "S\r\nBcc: attacker@evil.example".into(),
            text: "b".into(),
            html: None,
            attachments: vec![],
            from_name: None,
            reply_to: None,
        };
        let rec = RecordingEmailSender::default();
        assert!(matches!(rec.send(&m), Err(EmailError::Header("subject"))));
        assert_eq!(
            rec.messages().len(),
            0,
            "a refused message is never recorded"
        );
        // The disabled transport logs the recipient/subject, so it refuses too
        // rather than let a folded value forge a log line; it still reports.
        assert!(matches!(
            DisabledEmailSender.send(&m),
            Err(EmailError::Header("subject"))
        ));
        assert_eq!(DisabledEmailSender.transport(), "disabled");
        // The SMTP transport refuses before it opens a socket.
        let smtp = SmtpEmailSender::new(SmtpConfig {
            host: "127.0.0.1".into(),
            port: 1,
            from: "noreply@tucano.test".into(),
            username: None,
            password: None,
        });
        assert!(matches!(smtp.send(&m), Err(EmailError::Header("subject"))));
    }

    #[test]
    fn recipient_resolution_defaults_dedupes_and_validates() {
        // Absent/empty list -> the single customer default, exactly as before.
        let default = resolve_recipients(&[], Some("billing@tucanotime.test")).unwrap();
        assert_eq!(default, vec!["billing@tucanotime.test".to_string()]);
        assert!(resolve_recipients(&[], None).is_err());
        assert!(resolve_recipients(&[], Some("   ")).is_err());
        // Explicit order is preserved; a case-insensitive duplicate drops the
        // later occurrence and keeps the first spelling.
        let list = resolve_recipients(
            &[
                "Second@X.test".into(),
                "first@x.test".into(),
                " second@x.TEST ".into(),
            ],
            Some("ignored@tucanotime.test"),
        )
        .unwrap();
        assert_eq!(
            list,
            vec!["Second@X.test".to_string(), "first@x.test".to_string()]
        );
        // Control characters, a second '@' and empty entries are refused.
        for bad in ["a@b.co\r\nBcc: x@y.co", "two@@x.co", "", "   "] {
            let err = resolve_recipients(&[bad.into()], None).unwrap_err();
            assert_eq!(err[0].0, "recipients[0]", "{bad:?}");
        }
        let long = format!("{}@x.co", "a".repeat(320));
        assert!(resolve_recipients(&[long], None).is_err());
        // More than ten is refused outright.
        let many: Vec<String> = (0..11).map(|i| format!("u{i}@x.co")).collect();
        assert_eq!(
            resolve_recipients(&many, None).unwrap_err()[0].0,
            "recipients"
        );
    }

    /// M2: an AUTH failure must never echo the command payload (base64
    /// username/password) into the error that reaches a log or a 502 body.
    #[test]
    fn auth_failure_error_never_contains_credentials() {
        let user = "relay-operator@example.test";
        let pass = "smtp-password-SUPER-SECRET";
        let user_b64 = auth_line(user);
        let pass_b64 = auth_line(pass);

        let text = unexpected_reply(535, "AUTH <redacted>").to_string();
        assert!(text.contains("535"), "{text}");
        assert!(text.contains("AUTH <redacted>"), "{text}");
        for secret in [user, pass, user_b64.as_str(), pass_b64.as_str()] {
            assert!(!text.contains(secret), "credential leaked in {text}");
        }
        // Non-auth diagnostics keep the useful command line (it is not secret).
        let relay = unexpected_reply(550, "MAIL FROM:<noreply@example.test>").to_string();
        assert!(relay.contains("MAIL FROM"), "{relay}");
    }

    #[test]
    fn encrypted_smtp_uses_tls_and_starttls_never_downgrades_auth() {
        use std::io::{BufRead, BufReader, Read, Write};
        for implicit_tls in [true, false] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                if implicit_tls {
                    let mut header = [0u8; 3];
                    stream.read_exact(&mut header).unwrap();
                    assert_eq!(
                        header[0], 0x16,
                        "expected TLS handshake, not plaintext SMTP"
                    );
                    assert_eq!(header[1], 0x03);
                } else {
                    stream.write_all(b"220 local fixture ESMTP\r\n").unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    assert!(line.starts_with("EHLO "));
                    stream
                        .write_all(b"250-local fixture\r\n250 AUTH PLAIN LOGIN\r\n")
                        .unwrap();
                    line.clear();
                    assert_eq!(
                        reader.read_line(&mut line).unwrap(),
                        0,
                        "must disconnect before credentials when STARTTLS is unavailable"
                    );
                }
            });
            let config = SmtpConfig {
                host: "127.0.0.1".into(),
                port,
                from: "billing@example.test".into(),
                username: Some("private-user".into()),
                password: Some("private-password".into()),
            };
            let message = EmailMessage {
                to: vec!["recipient@example.test".into()],
                subject: "Local fixture".into(),
                text: "No external mail".into(),
                html: None,
                attachments: vec![],
                from_name: None,
                reply_to: None,
            };
            let error = SmtpEmailSender::new(config)
                .send_encrypted(&message, implicit_tls)
                .unwrap_err();
            server.join().unwrap();
            let text = error.to_string();
            for secret in ["private-user", "private-password"] {
                assert!(!text.contains(secret));
                assert!(!text.contains(&auth_line(secret)));
            }
        }
    }

    #[test]
    fn smtp_mime_uses_configured_from_address() {
        let message = EmailMessage {
            to: vec!["recipient@example.test".into()],
            subject: "Invoice".into(),
            text: "Body".into(),
            html: None,
            attachments: vec![],
            from_name: Some("Example Billing".into()),
            reply_to: None,
        };
        let wire = build_mime_with_sender(&message, Some("billing@example.test")).unwrap();
        assert!(wire.starts_with("From: Example Billing <billing@example.test>\r\n"));
        assert!(build_mime_with_sender(&message, Some("noreply@localhost")).is_ok());
        assert!(build_mime_with_sender(&message, Some("bad\r\nBcc: leak@example.test")).is_err());
    }

    /// M2 end-to-end: a relay that rejects `AUTH LOGIN` produces an error with
    /// the reply code, not the base64 username/password we sent.
    #[test]
    fn smtp_auth_rejection_does_not_echo_the_credentials() {
        use std::io::{BufRead, BufReader, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let user = "relay-operator";
        let pass = "relay-password-SUPER-SECRET";

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            stream.write_all(b"220 test ESMTP\r\n").unwrap();
            let mut line = String::new();
            reader.read_line(&mut line).unwrap(); // EHLO
            stream.write_all(b"250-test\r\n250 AUTH LOGIN\r\n").unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap(); // AUTH LOGIN
            stream.write_all(b"334 VXNlcm5hbWU6\r\n").unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap(); // base64 username
            stream.write_all(b"334 UGFzc3dvcmQ6\r\n").unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap(); // base64 password
            stream
                .write_all(b"535 5.7.8 Authentication credentials invalid\r\n")
                .unwrap();
            stream.flush().unwrap();
        });

        let sender = SmtpEmailSender::new(SmtpConfig {
            host: "127.0.0.1".into(),
            port,
            from: "noreply@example.test".into(),
            username: Some(user.into()),
            password: Some(pass.into()),
        });
        let message = EmailMessage {
            to: vec!["client@example.test".into()],
            subject: "test".into(),
            text: "body".into(),
            html: None,
            attachments: vec![],
            from_name: None,
            reply_to: None,
        };
        let err = sender.send(&message).unwrap_err();
        server.join().unwrap();

        let text = err.to_string();
        assert!(text.contains("535"), "{text}");
        assert!(text.contains("AUTH <redacted>"), "{text}");
        let user_b64 = auth_line(user);
        let pass_b64 = auth_line(pass);
        for secret in [user, pass, user_b64.as_str(), pass_b64.as_str()] {
            assert!(!text.contains(secret), "credential leaked in {text}");
        }
    }

    #[test]
    fn mime_joins_multiple_recipients_into_one_to_header() {
        let m = EmailMessage {
            to: vec!["one@x.co".into(), "two@x.co".into()],
            subject: "S".into(),
            text: "b".into(),
            html: None,
            attachments: vec![],
            from_name: None,
            reply_to: None,
        };
        let mime = build_mime(&m).unwrap();
        assert!(mime.contains("To: one@x.co, two@x.co\r\n"), "{mime}");
        // A control character in *any* recipient refuses the whole message.
        let hostile = EmailMessage {
            to: vec!["ok@x.co".into(), "bad@x.co\nBcc: x@y.co".into()],
            ..m
        };
        assert!(matches!(
            build_mime(&hostile),
            Err(EmailError::Header("recipient"))
        ));
    }
}
