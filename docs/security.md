# Security

This page describes how TucanoTime authenticates users, protects records,
stores provider credentials and records security-relevant events. Configuration
names and precedence are in [Configuration](configuration.md).

## Sessions and cookies

- A successful sign-in issues a stateless, HMAC-SHA256-signed token in the
  `tt_session` cookie: `HttpOnly`, `Path=/`, `SameSite=Lax`, with a 24-hour
  `Max-Age`.
- The `Secure` attribute is controlled independently by
  `TUCANO_SECURE_COOKIES`. It defaults to on when `TUCANO_ENV=production` and off
  otherwise. Set `TUCANO_SECURE_COOKIES=0` for a plain-HTTP LAN deployment, or
  the browser discards the cookie and login loops.
- The signing key is `TUCANO_SESSION_SECRET`, else
  `TUCANO_SESSION_SECRET_FILE`, else the auto-created `<data>/session.key`.
  In production, `TUCANO_SESSION_SECRET` shorter than 32 bytes is a hard
  failure. A too-short or unreadable `TUCANO_SESSION_SECRET_FILE` is ignored
  and the auto-created key is used instead; production refuses to start if that
  file cannot be persisted. Outside production shorter values are accepted with
  a warning. The generated key keeps sessions valid across restarts and
  container upgrades.
- Logging out records the token id in `<data>/revoked.json`; the file is checked
  on every request. Password, role and active-state changes increment the user's
  session version, which invalidates sessions issued before the change.
- The token is never stored server-side. A member who is deactivated or has
  their password changed is signed out on the next request.

## Passwords

Passwords are hashed with Argon2id and are never serialised to clients. Password
fields are masked and never prefilled. There is no invitation email and no
automated password-reset delivery: an administrator sets or changes a password
through **Settings → Users**.

## Roles and private records

- **Admin** — manages users, owns invoices, approves submissions and claims, and
  manages retainers, catalog, templates and configuration.
- **Member** — records and edits only their own time and expenses.

Entries and expenses carry the author's `user_id`. A member sees only their own
records; an admin sees everything. A mutation attempted on a record the caller
does not own returns **404**, not 403, so its existence is not leaked. Customers,
projects, tasks and categories are shared organisation-wide reference data.

Self-deletion, self-deactivation and self-demotion are blocked. The storage layer
also refuses to remove the last active administrator and enforces unique email
addresses under its writer lock. Deleting an account is permitted only when it
has no entries, expenses, submissions, claims or running timer; deactivate an
account with history instead.

## Single sign-on

SSO is additive to local password login.

- **OIDC / signed-token** is enabled by `oidc.client_secret` (vault) or
  `TUCANO_OIDC_CLIENT_SECRET`. The provider name is `oidc.issuer` /
  `TUCANO_OIDC_ISSUER`.
- **SAML** is a stub adapter. It requires both a pinned fingerprint
  (`saml.cert_fingerprint` / `TUCANO_SAML_CERT_FINGERPRINT`) and a shared signing
  secret (`saml.signing_secret` / `TUCANO_SAML_SIGNING_SECRET`); a fingerprint
  alone does not enable it.
- **Allowed domains** are a fail-closed allow-list. Only listed email domains
  are trusted; an empty or unset list trusts no domain. In that state the server
  logs a loud boot error, registers no provider and provisions nobody, while
  local login and the rest of the timesheet keep working.
- A verified assertion for a new user provisions a member account
  just-in-time; the IdP is the password store, so the local password is
  unusable. Provider secrets resolve vault first, environment second.
- The admin-group mapping is not read uniformly: see
  [Known limitations](configuration.md#known-limitations) for the difference
  between just-in-time provisioning and the role refresh of an existing account.

## Credential vault

Integration credentials entered in **Settings** (SMTP, OAuth tokens, ICS feed
URLs and similar) are encrypted at rest with AES-256-GCM in `<data>/secrets.bin`.
The master key is `<data>/vault.key`: 24 CSPRNG bytes as 32 base64url characters
(192 bits), written atomically with mode `0600` on first boot. A restored key
file that is group- or world-readable is tightened to `0600`; if that cannot be
applied, boot fails.

The fail-closed rules are deliberately asymmetric:

- A store with **no reachable key**, a **malformed** key (not exactly 32 bytes)
  or a **wrong** key is a hard boot failure. The app never silently regenerates a
  key over an existing store and never starts with an unreadable Settings tab.
- A failure to **write** a newly generated key (for example a read-only data
  directory) disables the vault for that boot and the server continues running.
  Nothing is stored in plaintext; fix the directory and restart.

`TUCANO_SECRET_KEY` / `TUCANO_SECRET_KEY_FILE` are an optional external override
that always wins and is never copied into the data directory. While an external
key or a valid `TUCANO_VAULT_ENABLED` is set, the vault is operator-managed: the
enable, disable and regenerate endpoints refuse with
`409 vault_managed_externally`. The key is never returned to a client or
logged; Settings shows only its source, the fixed file name `vault.key` and a
`sha256:…` fingerprint.

`vault_enabled` is read-only through `PUT /api/admin/config` (422
`validation_failed`). Only the dedicated lifecycle endpoints write it, and
disabling while any credential is stored is refused. Removing the last secret
deletes `secrets.bin`.

> **Backup trade-off.** The generated key sits beside the ciphertext, so a backup
> of the data directory contains both `secrets.bin` and `vault.key`. Protect the
> archive like the credentials it holds, keep an extra copy of the key
> elsewhere, or use an external key file outside `/data` and store that copy
> with the backup.

## Audit log

Security-relevant events are appended as JSON lines to `<data>/audit.log`:
`{"ts":…,"event":…,"subject":…}`. The log records event kind, subject (email or
id) and time only — never passwords, tokens or secret values. It rotates at
1 MiB with one previous generation (`audit.log.1`). Administrators can read the
recent tail through the audit endpoint.

## Rate limiting

Authentication failures are counted per identity in a fixed window: eight
failures within five minutes lock that key. A successful sign-in clears the
counter. The counter map is in-process and bounded to 10,000 keys; each write
prunes elapsed windows and evicts the oldest keys, so a flood of random
identities cannot exhaust memory. The limiter is per process, not shared across
replicas, which is appropriate for a single self-hosted server.

## Record locks and invoice permanence

Reads are never blocked. Only a write to a frozen record is refused, with
`409` and a reason.

| Provider | What it freezes | Released by |
| --- | --- | --- |
| `InvoiceLock` | Entries **and** expenses on any **non-draft** invoice | Nothing — invoiced records are permanent |
| `SubmissionLock` | Entries in a submitted or approved week | Rejecting or withdrawing a submitted week; an approved week is permanent |
| `ClaimLock` | Expenses on a submitted or approved reimbursement claim | Rejecting a submitted claim |

Draft invoices lock nothing and can be deleted. Once an invoice is issued, its
lines are frozen by the archived invoice document: settling (`paid` or
`partly_paid`) or writing off (`written_off`) does **not** release the entries,
because the timesheet must never drift from what was billed. The only way back to
editable is before issue.

`CombinedLocks` composes the three providers. They answer from one memoised
snapshot keyed on the source collections' directory mtimes, so a per-record check
does not re-read every invoice, submission or claim document, and a changed
directory forces a single re-scan. If the lock state cannot be verified, the
write is refused fail-closed with `503 lock_unavailable`; it is never allowed
because the check was inconclusive.

## Transport and deployment

- Put a TLS reverse proxy in front of the GUI when it is reachable beyond a
  trusted network, then leave `TUCANO_SECURE_COOKIES` at its production default.
- The container is hardened by default: read-only root, `/tmp` tmpfs, all
  capabilities dropped, `no-new-privileges`, non-root uid/gid 10001, and a
  loopback-only published port unless `TUCANO_HOST_ADDR=0.0.0.0` is set.
- The server holds `<data>/.server.lock`; a second instance against the same data
  directory refuses to start, so two schedulers cannot fire the same jobs.
- There is no hosted-checkout or payment-provider integration. Invoice payment
  details are the customer's own bank/transfer instructions, and settlement is
  recorded manually; no card data or provider credential exists in the system.

---

Last verified against TucanoTime 0.1.41.
