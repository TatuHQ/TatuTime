# TucanoTime on Unraid

A production-grade Unraid Docker template and runbook for **TucanoTime**, the
self-hosted timesheet server. The template is
[`my-TucanoTime.xml`](my-TucanoTime.xml); it runs the same distroless image the
project publishes from CI, with the same hardening the repository verifies:
read-only root filesystem, every capability dropped, no privilege escalation,
non-root uid **10001**, and only `/data` writable.

## Contents

- [What you get](#what-you-get)
- [1. Import the template](#1-import-the-template)
- [2. Field-by-field values (manual entry)](#2-field-by-field-values-manual-entry)
- [3. Image source options](#3-image-source-options)
- [4. Prepare the appdata folder and the vault key](#4-prepare-the-appdata-folder-and-the-vault-key)
- [5. First run](#5-first-run)
- [6. SMTP on Unraid](#6-smtp-on-unraid)
- [7. Security notes](#7-security-notes)
- [8. Backups and restore](#8-backups-and-restore)
- [9. Upgrades](#9-upgrades)
- [10. Troubleshooting](#10-troubleshooting)
- [11. Verified environment variables](#11-verified-environment-variables)

---

## What you get

| Item | Value |
|---|---|
| Image | `ghcr.io/tucanotechnology/tucanotime:latest` (public) |
| Runtime base | `gcr.io/distroless/cc-debian13:nonroot` — **no shell**, no package manager |
| User | `10001:10001` (numeric, fixed in the image) |
| Writable paths | `/data` (bind mount) and `/tmp` (tmpfs); nothing else |
| Port | container `8080`, host `8080` |
| State | JSON documents on `/mnt/user/appdata/tucanotime` — no database |
| Size | ~60 MB compressed image |

Because the image has no shell, Unraid's **Console** button will not open a
terminal. That is expected and is part of the hardening. Use
`docker logs TucanoTime` from an Unraid terminal or the Unraid log viewer.

> This repository cannot reach your Unraid host. Everything below that touches
> `/boot` or `/mnt/user` is a copy/paste step you perform on the server
> (Unraid terminal, SSH, or the flash SMB share).

---

## 1. Import the template

### Option A — copy the XML to the flash share (recommended)

1. From a machine on the LAN, open the flash share:

   ```
   \\192.168.2.252\flash
   ```

2. Copy [`my-TucanoTime.xml`](my-TucanoTime.xml) into:

   ```
   \\192.168.2.252\flash\config\plugins\dockerMan\templates-user\my-TucanoTime.xml
   ```

3. In the Unraid web UI open **Docker → Add Container**, choose
   **TucanoTime** from the **Template** drop-down, review the values, and
   click **Apply**.

On the Unraid terminal the same copy is:

```sh
cp /path/to/my-TucanoTime.xml \
   /boot/config/plugins/dockerMan/templates-user/my-TucanoTime.xml
```

### Option B — paste into the editor you already have open

Use the table in [section 2](#2-field-by-field-values-manual-entry). Add one
**Path**, one **Port** and the four **Variables**; then set **Extra Parameters**
exactly as shown. The Extra Parameters field is the one that makes the
container hardened, so do not skip it.

---

## 2. Field-by-field values (manual entry)

These are the literal values for the Unraid *Add Container* / *Edit* page.

| Unraid field | Type | Name / Target | Value |
|---|---|---|---|
| Name | — | container name | `TucanoTime` |
| Repository | — | image | `ghcr.io/tucanotechnology/tucanotime:latest` |
| Network Type | — | — | `Bridge` |
| Console shell command | — | — | `None` (the image has no shell) |
| Privileged | — | — | `false` (off) |
| Extra Parameters | — | — | `--read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges` |
| WebUI | — | — | `http://[IP]:[PORT:8080]/` |
| Data | Path | `/data` | `/mnt/user/appdata/tucanotime` (read/write) |
| WebUI port | Port | `8080` (TCP) | `8080` |
| Data dir (internal) | Variable | `TUCANO_DATA_DIR` | `/data` |
| Listen port (internal) | Variable | `TUCANO_PORT` | `8080` |
| Plain-HTTP cookies | Variable | `TUCANO_SECURE_COOKIES` | `0` |
| Vault key file | Variable | `TUCANO_SECRET_KEY_FILE` | *(optional; leave empty)* |

Notes:

- **Do not** add a container path for the vault key: the app creates
  `/data/vault.key` inside the existing `Data` path, so no extra mount is
  needed.
- The **Vault key file** variable is an **optional external override**. Empty
  means the app generates and manages `/data/vault.key` itself (see
  [section 4](#4-prepare-the-appdata-folder-and-the-vault-key)).
- While an override is set (`TUCANO_SECRET_KEY_FILE`, `TUCANO_SECRET_KEY` or a
  valid `TUCANO_VAULT_ENABLED`), the environment owns the vault lifecycle: the
  Settings enable/disable/regenerate actions are unavailable and the API
  answers **409 `vault_managed_externally`**. Remove the variable to hand the
  lifecycle back to the app.
- `TUCANO_DATA_DIR` and `TUCANO_PORT` are already baked into the image
  (`ENV` in the [`Dockerfile`](../../Dockerfile)); they are surfaced in the
  template so the mapping is visible and a port change stays consistent.
- There is **no** `TUCANO_SECRET_KEY_FILE` mount point in the template UI; when
  you use the override, mount the key file yourself and give its path *inside*
  the container.

---

## 3. Image source options

### A. Pull from the project's registry (default)

CI publishes on every push to `main`
([`.github/workflows/ci.yml`](../../.github/workflows/ci.yml)):

```
ghcr.io/tucanotechnology/tucanotime:latest
ghcr.io/tucanotechnology/tucanotime:sha-<short-sha>
```

The package is **public**: an anonymous `docker manifest inspect` succeeds, so
Unraid does **not** need registry credentials to pull `:latest`. Example
reproducible pin:

```
ghcr.io/tucanotechnology/tucanotime:sha-53c8da9
```

Use the `sha-`-prefixed tag, not a bare SHA, when you want a fixed build.

If the package is ever made private, authenticate on Unraid once and pull:

```sh
docker login ghcr.io -u <github-user> -p <PAT-with-read:packages>
docker pull ghcr.io/tucanotechnology/tucanotime:latest
```

### B. Build the image on Unraid from source

The repository has no build step for the GUI (vanilla HTML/CSS/JS embedded at
compile time), so the build only needs Docker and network access:

```sh
cd /mnt/user/appdata   # or any scratch dir large enough for target/
git clone https://github.com/TucanoTechnology/TucanoTime.git
cd TucanoTime
docker build -t tucanotime:unraid .
```

The final image is about **60 MB**. Then set the template **Repository** to
`tucanotime:unraid`. Unraid refuses to start a container if it cannot resolve
the image in a registry, so keep the local tag present (build before *Apply*,
or `docker tag tucanotime:unraid tucanotime:latest`) and use **Apply** without
forcing a pull.

### C. Offline transfer

```sh
# on a machine that can pull/build
docker save ghcr.io/tucanotechnology/tucanotime:latest -o tucanotime.tar
# on Unraid
docker load -i tucanotime.tar
```

---

## 4. Prepare the appdata folder and the vault key

The image runs as uid/gid **10001** and never changes identity, so the host
folder must be owned by that uid. Run this once, before the first start:

```sh
mkdir -p /mnt/user/appdata/tucanotime
chown -R 10001:10001 /mnt/user/appdata/tucanotime
```

**No manual key is needed.** The **credential vault** (Settings → Email and
every other integration secret) is encrypted at rest with AES-256-GCM under a
master key the app generates automatically on first boot at
`/mnt/user/appdata/tucanotime/vault.key`: 24 random bytes as 32 base64url
characters (192 bits), written atomically with mode 0600 by uid 10001. The
`chown` above is the only preparation the key needs.

The **Vault key file** field in the template (`TUCANO_SECRET_KEY_FILE`) is an
*optional* external override and is empty by default. Set it only when the key
must live outside the data directory — for example a Docker secret mounted
into the container. The file it names must hold exactly 32 ASCII bytes (a
trailing newline is trimmed); an external key always wins and is never copied
into `/data`. With no override, the app uses the key file it generated.

While either key variable (or a valid `TUCANO_VAULT_ENABLED`) is set, the
vault is **operator-managed**: the Settings actions do not change the key and
the API refuses them with **409 `vault_managed_externally`**. Remove the
variable to restore the Settings controls.

If a restored `vault.key` is group- or world-readable, the app tightens it to
`0600` automatically on the next start. If it cannot (for example an immutable
file on a read-only mount), the container **refuses to start** with an operator
message rather than running with a key other local users can read.

Verify a key after a restore from **Settings → Security**: the *Vault key*
card shows the source (`key_file` for the generated key), the file name
`vault.key` and a `sha256:…` fingerprint. The fingerprint identifies the key
without exposing it — the raw key is never returned by the API or the GUI — and
the same fingerprint is logged at boot. If the restored fingerprint differs
from the one recorded with the backup, `secrets.bin` will not open (and the app
will refuse to start).

If the app cannot write the key (for example a read-only or wrongly owned
`/data`), it still starts with the vault **disabled** and reports
`vault_disabled`; fix the ownership of `/data` and restart. If `secrets.bin`
exists and no key can open it — no key file and no external override, a
malformed key (not exactly 32 bytes) or the wrong key — the app **refuses to
start** rather than silently showing an empty Settings tab.

> **Backup trade-off:** the generated `vault.key` lives inside the appdata
> folder, so a copy of that folder contains both the ciphertext
> (`secrets.bin`) and its key. Protect that backup like the credentials it
> contains, keep an extra copy of `vault.key` elsewhere, or supply the key
> externally (`TUCANO_SECRET_KEY_FILE` mounted from outside `/data`) and store
> that copy with the backup. Without the key the stored credentials are
> permanently unreadable.

---

## 5. First run

1. Start the container and open:

   ```
   http://192.168.2.252:8080/
   ```

2. Complete the **admin startup wizard**: create the administrator account,
   then the company profile, then the base currency. The first account created
   is the admin; after that every data route requires a session.
3. Open **Settings → Email** and configure SMTP (see the next section).
4. Optional: **Settings → Users** for additional accounts, and
   **Settings → Security** for SSO / session policy.

The API docs are at `http://192.168.2.252:8080/docs` and the machine-readable
contract at `/openapi.json`.

---

## 6. SMTP on Unraid

TucanoTime's SMTP client is deliberately **plaintext**: it opens a raw TCP
connection and supports optional `AUTH LOGIN`, with **no TLS and no
STARTTLS** (see [`src/email.rs`](../../src/email.rs)). A hosted provider on
port 465 (implicit TLS) or 587 (STARTTLS) therefore will **not** work when
contacted directly.

You have two practical options.

### Option A — a local relay that adds TLS (production)

Run a relay container on Unraid that accepts plaintext SMTP from TucanoTime and
forwards with TLS to your provider. `boky/postfix` is a common choice:

```sh
docker run -d --name tucano-relay --restart unless-stopped \
  -p 2525:25 \
  -e RELAYHOST='[smtp.your-provider.tld]:587' \
  -e RELAYHOST_USERNAME='your-provider-username' \
  -e RELAYHOST_PASSWORD='your-provider-password' \
  -e ALLOWED_SENDER_DOMAINS='your-domain.tld' \
  boky/postfix
```

Set the relay host's TLS policy according to the image's documentation (the
`boky/postfix` image maps `POSTFIX_*` environment variables into `main.cf`;
enable TLS for the outbound hop, for example with
`-e POSTFIX_smtp_tls_security_level=encrypt`). Then in **Settings → Email**
use:

| Setting | Value |
|---|---|
| SMTP host | `192.168.2.252` (the Unraid host) |
| SMTP port | `2525` |
| From address | an address your relay is allowed to send as |
| Username / password | usually blank — the relay authenticates upstream |

If you prefer environment variables over the vault, the equivalent names are
`TUCANO_SMTP_HOST`, `TUCANO_SMTP_PORT`, `TUCANO_SMTP_FROM`,
`TUCANO_SMTP_USERNAME` and `TUCANO_SMTP_PASSWORD` (the vault wins when both
are set; see [section 11](#11-verified-environment-variables)).

### Option B — MailHog (testing only)

```sh
docker run -d --name mailhog -p 1025:1025 -p 8025:8025 mailhog/mailhog
```

Point **Settings → Email** at host `192.168.2.252`, port `1025`, and read the
captured mail at:

```
http://192.168.2.252:8025
```

MailHog never delivers anything; use it to verify templates, attachments and
the invoice/reminder flows without touching a real mailbox.

> If SMTP is not configured at all, the app does not fail: it logs the message
> and reports the `disabled` transport. Configure a relay before relying on
> invoice or reminder email.

---

## 7. Security notes

- **LAN exposure.** In bridge mode the published port is reachable from the
  whole LAN by default. The server has a first-run bootstrap endpoint that
  creates the first administrator, so start it on a trusted network, create
  the admin immediately, and firewall the port if the LAN is not trusted.
- **TLS.** Put a TLS reverse proxy in front of the WebUI when it is exposed
  beyond a trusted LAN, and then set `TUCANO_SECURE_COOKIES=1`. Over plain
  HTTP keep it at `0`, otherwise the browser discards the session cookie and
  login loops.
- **Read-only root.** `--read-only` is intentional. Only the `/data` bind
  mount and the `/tmp` tmpfs accept writes; the binary itself needs nothing
  else. Upgrades work normally because the image is replaced, not modified.
- **Capabilities.** `--cap-drop ALL` removes every Linux capability;
  `--security-opt no-new-privileges` blocks setuid/setgid escalation.
- **No shell.** The distroless image cannot be entered with `docker exec ...
  sh`; the Console button fails by design. Logs are the debugging interface.
- **Non-root.** The process is uid/gid 10001. Do not run the template as root
  to "fix" permissions — fix the ownership of `/data` instead.
- **One instance only.** The server holds an exclusive lock in the data
  directory; never point two containers at the same appdata folder.
- **Vault key.** The app creates `vault.key` (0600) in the data directory on
  first boot, and tightens a restored copy to 0600 if it is group/world-readable
  (refusing to boot when it cannot). Anyone who can read both `secrets.bin` and
  `vault.key` can decrypt every integration credential, so protect the appdata
  share and any backup; use the external `TUCANO_SECRET_KEY_FILE` override if
  the key must live elsewhere — that also makes the Settings key actions
  unavailable (409 `vault_managed_externally`).

---

## 8. Backups and restore

Everything the installation knows lives in one folder:

```
/mnt/user/appdata/tucanotime
```

That includes the JSON documents (customers, projects, entries, invoices,
users, expenses, notifications), `config.json`, `session.key`, `audit.log`,
scheduler state, and the encrypted vault `secrets.bin` together with the
generated `vault.key`. A copy of the folder is a complete backup **including
the key** — protect it accordingly. **Keep an extra copy of `vault.key`
separately**: losing it makes `secrets.bin` permanently unreadable. If you
supply the key externally with `TUCANO_SECRET_KEY[_FILE]`, back that file up
instead; it is not part of the appdata folder.

The image ships an operator backup/restore CLI on the same binary. Back up
while the server is stopped (or accept a torn snapshot across collections):

```sh
docker stop TucanoTime

docker run --rm \
  -v /mnt/user/appdata/tucanotime:/data \
  -v /mnt/user/backups:/backups \
  ghcr.io/tucanotechnology/tucanotime:latest \
  --backup /backups/tucanotime-$(date +%F).tar.gz

docker start TucanoTime
```

Restore (stop the server first; the CLI refuses a non-empty data dir unless
`--force` is given):

```sh
docker stop TucanoTime

docker run --rm \
  -v /mnt/user/appdata/tucanotime:/data \
  -v /mnt/user/backups:/backups \
  ghcr.io/tucanotechnology/tucanotime:latest \
  --restore /backups/tucanotime-2026-10-19.tar.gz

docker start TucanoTime
```

Archives carry a `manifest.json` with a version and per-file sha256, and
restores verify checksums before writing. The `/backups` folder must be
writable by uid 10001.

---

## 9. Upgrades

- **`:latest`**: pull the new image and recreate the container. In the Unraid
  UI use **Docker → Check for Updates** or **Force Update**; everything in
  `/data` survives. On the terminal:

  ```sh
  docker pull ghcr.io/tucanotechnology/tucanotime:latest
  # then recreate TucanoTime with the same template values
  ```

- **Pinned**: set the Repository to a `sha-<short-sha>` tag and change it
  deliberately when you want a new version.
- `--read-only` does not hinder upgrades: the container filesystem is thrown
  away and rebuilt from the new image. Nothing outside `/data` persists by
  design, so do not store state in the container layer.

---

## 10. Troubleshooting

| Symptom | Cause and fix |
|---|---|
| Unraid shows the container as **unhealthy** | The image HEALTHCHECK runs `tucano-time --health`, which probes `http://127.0.0.1:8080/healthz` **inside** the container. Check `docker logs TucanoTime`; verify the internal `TUCANO_PORT` is still `8080`. From the Unraid host, `curl -i http://127.0.0.1:8080/healthz` (or the host port you mapped). |
| **Login loops** back to the sign-in page after an upgrade | The session cookie carries `Secure` but the browser is on plain `http://`. Set `TUCANO_SECURE_COOKIES=0` (the template default), recreate, and log in again. |
| **Permission denied** writing `/data` | The bind mount is not owned by uid 10001. Run `chown -R 10001:10001 /mnt/user/appdata/tucanotime`. Unraid's *New Permissions* tool can reset ownership to `nobody:users`; re-run the chown after it. |
| **`vault_disabled`** in Settings → Email | The vault is disabled by configuration (`TUCANO_VAULT_ENABLED=false` or `vault_enabled: false` in `config.json`), or the app could not write `/data/vault.key` because the data directory is not writable by uid 10001. Check the *Vault key* card in Settings → Security: `config`/`env` means it was switched off deliberately; `write_failed` means fix ownership/permissions and restart. |
| **Vault actions disabled / 409 `vault_managed_externally`** | `TUCANO_VAULT_ENABLED`, `TUCANO_SECRET_KEY` or `TUCANO_SECRET_KEY_FILE` is set, so the environment owns the lifecycle. Change or remove that variable (or the external key file) — the Settings enable/disable/regenerate actions only work when the app manages `/data/vault.key`. |
| Container **refuses to start** with "vault store found on disk ... key is missing" | `secrets.bin` exists but no key can open it. Restore the original `vault.key` (or the backup that contains it), or point `TUCANO_SECRET_KEY_FILE` at the original external key; do not delete `secrets.bin` unless you accept losing the stored credentials. |
| Container **refuses to start** with "TUCANO_SECRET_KEY is configured but invalid" | An external key is not exactly 32 ASCII characters. Generated keys are always 32 characters; for a manual one, `openssl rand -hex 16` prints 32 characters (`-hex 32` prints 64 and is rejected). |
| **Console / `docker exec` fails** | Expected: distroless has no shell. Use `docker logs -f TucanoTime`. |
| **Port already in use** | Change the host side of the WebUI port mapping (for example `8081`), keep the container side at `8080`, and update the WebUI URL. |
| Second container on the same appdata exits immediately | The single-instance guard (`.server.lock`) refuses a second server on the same data directory. Run one container per appdata folder. |
| Emails are logged but never sent | No SMTP host is configured, or the host/port is wrong. The app speaks plaintext SMTP only; use the relay from section 6. |

Useful commands (Unraid terminal):

```sh
docker logs --tail 200 TucanoTime
docker inspect --format '{{.State.Health.Status}}' TucanoTime
docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' TucanoTime
```

---

## 11. Verified environment variables

Every variable below is read by the binary (grep of `src/` at v0.1.40), with
the file and line where it is read. Only the first four appear in the Unraid
template; the rest are optional and can be supplied through the template's
**Extra Parameters** (`-e NAME=value`) or left to `config.json` / the vault.

| Variable | Read at | Format / default |
|---|---|---|
| `TUCANO_DATA_DIR` | [`src/main.rs:17`](../../src/main.rs#L17) | path, default `Data`; the image sets `/data` |
| `TUCANO_PORT` | [`src/main.rs:54`](../../src/main.rs#L54), [`src/main.rs:124`](../../src/main.rs#L124) | TCP port, default `8080` |
| `TUCANO_ENV` | [`src/main.rs:153`](../../src/main.rs#L153) | `production` enables the strict secret policy and Secure cookies by default |
| `TUCANO_SECURE_COOKIES` | [`src/main.rs:162`](../../src/main.rs#L162) | `0` / `false` disables the cookie `Secure` flag; empty means "the `TUCANO_ENV` default" |
| `TUCANO_SESSION_SECRET` | [`src/main.rs:156`](../../src/main.rs#L156), [`src/auth.rs:202`](../../src/auth.rs#L202) | at least 32 bytes in production |
| `TUCANO_SESSION_SECRET_FILE` | [`src/main.rs:157`](../../src/main.rs#L157), [`src/auth.rs:205`](../../src/auth.rs#L205) | path; file contents trimmed; at least 32 bytes in production |
| `TUCANO_SECRET_KEY` | [`src/main.rs:180`](../../src/main.rs#L180), [`src/vault.rs:254`](../../src/vault.rs#L254) | optional override; **exactly 32 bytes** (ASCII), AES-256-GCM vault key; makes the Settings key actions 409 `vault_managed_externally` |
| `TUCANO_SECRET_KEY_FILE` | [`src/main.rs:181`](../../src/main.rs#L181), [`src/vault.rs:254`](../../src/vault.rs#L254) | optional override; path to a file whose trimmed contents are **exactly 32 bytes**; same 409 behaviour |
| `TUCANO_VAULT_ENABLED` | [`src/main.rs:174`](../../src/main.rs#L174), [`src/appconfig.rs:161`](../../src/appconfig.rs#L161) | valid `true`/`false` (default `true`); `false` disables the vault (a `secrets.bin` on disk then refuses to start); a valid value makes the lifecycle operator-managed (409 `vault_managed_externally`) |
| `TUCANO_MAX_DOCS` | [`src/main.rs:182`](../../src/main.rs#L182), [`src/store/mod.rs:385`](../../src/store/mod.rs#L385) | integer, default `50000`, range 10..1,000,000 |
| `TUCANO_REMINDER_DAYS` | [`src/appconfig.rs:56`](../../src/appconfig.rs#L56) | integer 1..365, default `7` |
| `TUCANO_ORG_NAME` | [`src/appconfig.rs:66`](../../src/appconfig.rs#L66) | string, default `TucanoTime` |
| `TUCANO_BASE_CURRENCY` | [`src/appconfig.rs:134`](../../src/appconfig.rs#L134) | ISO 4217 code, e.g. `EUR`; unset disables FX display |
| `TUCANO_SSO_ADMIN_GROUP` | [`src/appconfig.rs:74`](../../src/appconfig.rs#L74), [`src/api/auth.rs:271`](../../src/api/auth.rs#L271) | string, default empty |
| `TUCANO_SSO_ALLOWED_DOMAINS` | [`src/appconfig.rs:80`](../../src/appconfig.rs#L80), [`src/sso.rs:313`](../../src/sso.rs#L313) | comma-separated domains; empty is fail-closed |
| `TUCANO_CALENDAR_OAUTH_REDIRECT` | [`src/appconfig.rs:86`](../../src/appconfig.rs#L86) | string, default `/api/calendar/oauth/callback` |
| `TUCANO_GOOGLE_CLIENT_ID` | [`src/appconfig.rs:94`](../../src/appconfig.rs#L94), [`src/calendar_oauth.rs:379`](../../src/calendar_oauth.rs#L379) | OAuth client id (public half) |
| `TUCANO_MS_CLIENT_ID` | [`src/appconfig.rs:100`](../../src/appconfig.rs#L100), [`src/calendar_oauth.rs:401`](../../src/calendar_oauth.rs#L401) | OAuth client id (public half) |
| `TUCANO_QBO_BASE_URL` | [`src/appconfig.rs:106`](../../src/appconfig.rs#L106), [`src/accounting.rs:85`](../../src/accounting.rs#L85) | URL override |
| `TUCANO_XERO_BASE_URL` | [`src/appconfig.rs:112`](../../src/appconfig.rs#L112), [`src/accounting.rs:100`](../../src/accounting.rs#L100) | URL override |
| `TUCANO_FX_RATES_FILE` | [`src/fx.rs:23`](../../src/fx.rs#L23) | path to an offline rates document |
| `TUCANO_WEB_DIR` | [`src/assets.rs:52`](../../src/assets.rs#L52) | **development only**: serve assets from disk |
| `TUCANO_SMTP_HOST` | [`src/email.rs:279`](../../src/email.rs#L279) | hostname; unset disables SMTP |
| `TUCANO_SMTP_PORT` | [`src/email.rs:280`](../../src/email.rs#L280) | integer, default `25` |
| `TUCANO_SMTP_FROM` | [`src/email.rs:284`](../../src/email.rs#L284) | address, default `noreply@localhost` |
| `TUCANO_SMTP_USERNAME` | [`src/email.rs:289`](../../src/email.rs#L289) | string; with a password enables `AUTH LOGIN` |
| `TUCANO_SMTP_PASSWORD` | [`src/email.rs:290`](../../src/email.rs#L290) | string; **set it in the vault, not the template** |
| `TUCANO_OIDC_CLIENT_SECRET` | [`src/sso.rs:330`](../../src/sso.rs#L330) | string |
| `TUCANO_OIDC_ISSUER` | [`src/sso.rs:348`](../../src/sso.rs#L348) | string, default `oidc` |
| `TUCANO_SAML_CERT_FINGERPRINT` | [`src/sso.rs:331`](../../src/sso.rs#L331) | string |
| `TUCANO_SAML_SIGNING_SECRET` | [`src/sso.rs:332`](../../src/sso.rs#L332) | string |
| `TUCANO_SAML_ISSUER` | [`src/sso.rs:360`](../../src/sso.rs#L360) | string, default `saml` |
| `TUCANO_CALENDAR_ICS` | [`src/api/timer_calendar.rs:145`](../../src/api/timer_calendar.rs#L145) | http(s) URL or local file path |
| `TUCANO_CALENDAR_PROVIDER` | [`src/calendar_oauth.rs:355`](../../src/calendar_oauth.rs#L355) | `google` or `microsoft` |
| `TUCANO_GOOGLE_TOKEN` / `TUCANO_GOOGLE_REFRESH_TOKEN` | [`src/calendar_oauth.rs:372`](../../src/calendar_oauth.rs#L372), [`src/calendar_oauth.rs:377`](../../src/calendar_oauth.rs#L377) | OAuth tokens |
| `TUCANO_GOOGLE_CALENDAR` | [`src/calendar_oauth.rs:373`](../../src/calendar_oauth.rs#L373) | calendar id, default `primary` |
| `TUCANO_GOOGLE_CLIENT_SECRET` | [`src/calendar_oauth.rs:382`](../../src/calendar_oauth.rs#L382) | string |
| `TUCANO_MS_TOKEN` / `TUCANO_MS_REFRESH_TOKEN` | [`src/calendar_oauth.rs:396`](../../src/calendar_oauth.rs#L396), [`src/calendar_oauth.rs:398`](../../src/calendar_oauth.rs#L398) | OAuth tokens |
| `TUCANO_MS_CLIENT_SECRET` | [`src/calendar_oauth.rs:404`](../../src/calendar_oauth.rs#L404) | string |
| `TUCANO_QBO_TOKEN` | [`src/accounting.rs:79`](../../src/accounting.rs#L79) | OAuth token |
| `TUCANO_XERO_TOKEN` | [`src/accounting.rs:93`](../../src/accounting.rs#L93) | OAuth token |

`RUST_LOG` is also honoured by the tracing subscriber (default `info`).

Neither vault key variable is required: the app generates and manages
`<data>/vault.key` on its own. When an external key is wanted, prefer
`TUCANO_SECRET_KEY_FILE` over the plain `TUCANO_SECRET_KEY` (and likewise
`TUCANO_SESSION_SECRET_FILE` over `TUCANO_SESSION_SECRET`): the value never
appears in the Unraid template XML or `docker inspect`. Setting either (or
`TUCANO_VAULT_ENABLED`) makes the vault operator-managed: the Settings
enable/disable/regenerate actions are refused with 409
`vault_managed_externally` until the variable is removed.
