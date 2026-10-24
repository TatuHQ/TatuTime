# TatuTime on Unraid

A production-grade Unraid Docker template and runbook for **TatuTime**, the
self-hosted timesheet server. The template is
[`my-TatuTime.xml`](my-TatuTime.xml); it runs the same distroless image the
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
- [11. Environment variables on Unraid](#11-environment-variables-on-unraid)

---

## What you get

| Item | Value |
|---|---|
| Image | `ghcr.io/tatuhq/tatutime:latest` (published by CI; pin a release with `:<version>`) |
| Runtime base | `gcr.io/distroless/cc-debian13:nonroot` — **no shell**, no package manager |
| User | `10001:10001` (numeric, fixed in the image) |
| Writable paths | `/data` (bind mount) and `/tmp` (tmpfs); nothing else |
| Port | container `8080`, host `8080` |
| State | JSON documents on `/mnt/user/appdata/tatutime` — no database |

Because the image has no shell, Unraid's **Console** button will not open a
terminal. That is expected and is part of the hardening. Use
`docker logs TatuTime` from an Unraid terminal or the Unraid log viewer.

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

2. Copy [`my-TatuTime.xml`](my-TatuTime.xml) into:

   ```
   \\192.168.2.252\flash\config\plugins\dockerMan\templates-user\my-TatuTime.xml
   ```

3. In the Unraid web UI open **Docker → Add Container**, choose
   **TatuTime** from the **Template** drop-down, review the values, and
   click **Apply**.

On the Unraid terminal the same copy is:

```sh
cp /path/to/my-TatuTime.xml \
   /boot/config/plugins/dockerMan/templates-user/my-TatuTime.xml
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
| Name | — | container name | `TatuTime` |
| Repository | — | image | `ghcr.io/tatuhq/tatutime:latest` |
| Network Type | — | — | `Bridge` |
| Console shell command | — | — | `None` (the image has no shell) |
| Privileged | — | — | `false` (off) |
| Extra Parameters | — | — | `--read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges` |
| WebUI | — | — | `http://[IP]:[PORT:8080]/` |
| Data | Path | `/data` | `/mnt/user/appdata/tatutime` (read/write) |
| WebUI port | Port | `8080` (TCP) | `8080` |
| Data dir (internal) | Variable | `TATU_DATA_DIR` | `/data` |
| Listen port (internal) | Variable | `TATU_PORT` | `8080` |
| Plain-HTTP cookies | Variable | `TATU_SECURE_COOKIES` | `0` |
| Vault key file | Variable | `TATU_SECRET_KEY_FILE` | *(optional; leave empty)* |

Notes:

- **Do not** add a container path for the vault key: the app creates
  `/data/vault.key` inside the existing `Data` path, so no extra mount is
  needed.
- The **Vault key file** variable is an **optional external override**. Empty
  means the app generates and manages `/data/vault.key` itself (see
  [section 4](#4-prepare-the-appdata-folder-and-the-vault-key)).
- While an override is set (`TATU_SECRET_KEY_FILE`, `TATU_SECRET_KEY` or a
  valid `TATU_VAULT_ENABLED`), the environment owns the vault lifecycle: the
  Settings enable/disable/regenerate actions are unavailable and the API
  answers **409 `vault_managed_externally`**. Remove the variable to hand the
  lifecycle back to the app.
- `TATU_DATA_DIR` and `TATU_PORT` are already baked into the image
  (`ENV` in the [`Dockerfile`](../../Dockerfile)); they are surfaced in the
  template so the mapping is visible and a port change stays consistent.
- There is **no** `TATU_SECRET_KEY_FILE` mount point in the template UI; when
  you use the override, mount the key file yourself and give its path *inside*
  the container.

---

## 3. Image source options

### A. Pull from the project's registry (default)

CI publishes on every push to `main`
([`.github/workflows/ci.yml`](../../.github/workflows/ci.yml)):

```
ghcr.io/tatuhq/tatutime:latest
ghcr.io/tatuhq/tatutime:<version>
ghcr.io/tatuhq/tatutime:sha-<short-sha>
```

Published at `ghcr.io/tatuhq/tatutime` as `:latest`, `:<version>`
(the crate version of the release, the same number as the `v<version>` git tag)
and `:sha-<short>`. Check the registry package visibility: when it is public, an
anonymous `docker manifest inspect` succeeds and Unraid does **not** need
registry credentials to pull `:latest`. Example reproducible pin:

```
ghcr.io/tatuhq/tatutime:<version>
```

Use `:<version>` to follow releases, or the `sha-`-prefixed tag (never a bare
SHA) when you want one exact build.

If the package is private, authenticate on Unraid once and pull:

```sh
docker login ghcr.io -u <github-user> -p <PAT-with-read:packages>
docker pull ghcr.io/tatuhq/tatutime:latest
```

### B. Build the image on Unraid from source

The repository has no build step for the GUI (vanilla HTML/CSS/JS embedded at
compile time), so the build only needs Docker and network access:

```sh
cd /mnt/user/appdata   # or any scratch dir large enough for target/
git clone https://github.com/TatuHQ/TatuTime.git
cd TatuTime
docker build -t tatutime:unraid .
```

Measure the result with `docker image ls tatutime:unraid` if you need the
compressed size. Then set the template **Repository** to `tatutime:unraid`.
Unraid refuses to start a container if it cannot resolve
the image in a registry, so keep the local tag present (build before *Apply*,
or `docker tag tatutime:unraid tatutime:latest`) and use **Apply** without
forcing a pull.

### C. Offline transfer

```sh
# on a machine that can pull/build
docker save ghcr.io/tatuhq/tatutime:latest -o tatutime.tar
# on Unraid
docker load -i tatutime.tar
```

---

## 4. Prepare the appdata folder and the vault key

The image runs as uid/gid **10001** and never changes identity, so the host
folder must be owned by that uid. Run this once, before the first start:

```sh
mkdir -p /mnt/user/appdata/tatutime
chown -R 10001:10001 /mnt/user/appdata/tatutime
```

**No manual key is needed.** The **credential vault** (Settings → Email and
every other integration secret) is encrypted at rest with AES-256-GCM under a
master key the app generates automatically on first boot at
`/mnt/user/appdata/tatutime/vault.key`: 24 random bytes as 32 base64url
characters (192 bits), written atomically with mode 0600 by uid 10001. The
`chown` above is the only preparation the key needs.

The **Vault key file** field in the template (`TATU_SECRET_KEY_FILE`) is an
*optional* external override and is empty by default. Set it only when the key
must live outside the data directory — for example a Docker secret mounted
into the container. The file it names must hold exactly 32 bytes (the generated
key is 32 base64url characters; a trailing newline is trimmed); an external key
always wins and is never copied
into `/data`. With no override, the app uses the key file it generated.

While either key variable (or a valid `TATU_VAULT_ENABLED`) is set, the
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
> externally (`TATU_SECRET_KEY_FILE` mounted from outside `/data`) and store
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

TatuTime's SMTP client is deliberately **plaintext**: it opens a raw TCP
connection and supports optional `AUTH LOGIN`, with **no TLS and no
STARTTLS** (see [`src/email.rs`](../../src/email.rs)). A hosted provider on
port 465 (implicit TLS) or 587 (STARTTLS) therefore will **not** work when
contacted directly.

You have two practical options.

### Option A — a local relay that adds TLS (production)

Run a relay container on Unraid that accepts plaintext SMTP from TatuTime and
forwards with TLS to your provider. `boky/postfix` is a common choice:

```sh
docker run -d --name tatu-relay --restart unless-stopped \
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
`TATU_SMTP_HOST`, `TATU_SMTP_PORT`, `TATU_SMTP_FROM`,
`TATU_SMTP_USERNAME` and `TATU_SMTP_PASSWORD` (the vault wins when both
are set; see [section 11](#11-environment-variables-on-unraid)).

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
  beyond a trusted LAN, and then set **both** `TATU_ENV=production` and
  `TATU_SECURE_COOKIES=1`. The template's `TATU_SECURE_COOKIES=0` overrides
  the Secure-cookie default that `TATU_ENV=production` would otherwise enable.
  Over plain HTTP keep `TATU_SECURE_COOKIES=0` (the template default) and do
  not set `TATU_ENV=production`, otherwise the browser discards the session
  cookie and login loops.
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
  share and any backup; use the external `TATU_SECRET_KEY_FILE` override if
  the key must live elsewhere — that also makes the Settings key actions
  unavailable (409 `vault_managed_externally`).

---

## 8. Backups and restore

Everything the installation knows lives in one folder:

```
/mnt/user/appdata/tatutime
```

That includes the JSON documents (customers, projects, entries, invoices,
users, expenses, notifications), `config.json`, `session.key`, `audit.log`,
scheduler state, and the encrypted vault `secrets.bin` together with the
generated `vault.key`. A copy of the folder is a complete backup **including
the key** — protect it accordingly. **Keep an extra copy of `vault.key`
separately**: losing it makes `secrets.bin` permanently unreadable. If you
supply the key externally with `TATU_SECRET_KEY[_FILE]`, back that file up
instead; it is not part of the appdata folder.

The image ships an operator backup/restore CLI on the same binary. Back up
while the server is stopped (or accept a torn snapshot across collections):

```sh
docker stop TatuTime

docker run --rm \
  -v /mnt/user/appdata/tatutime:/data \
  -v /mnt/user/backups:/backups \
  ghcr.io/tatuhq/tatutime:latest \
  --backup /backups/tatutime-$(date +%F).tar.gz

docker start TatuTime
```

Restore (stop the server first; the CLI refuses a non-empty data dir unless
`--force` is given):

```sh
docker stop TatuTime

docker run --rm \
  -v /mnt/user/appdata/tatutime:/data \
  -v /mnt/user/backups:/backups \
  ghcr.io/tatuhq/tatutime:latest \
  --restore /backups/tatutime-2026-10-19.tar.gz

docker start TatuTime
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
  docker pull ghcr.io/tatuhq/tatutime:latest
  # then recreate TatuTime with the same template values
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
| Unraid shows the container as **unhealthy** | The image HEALTHCHECK runs `tatu-time --health`, which probes `http://127.0.0.1:8080/healthz` **inside** the container. Check `docker logs TatuTime`; verify the internal `TATU_PORT` is still `8080`. From the Unraid host, `curl -i http://127.0.0.1:8080/healthz` (or the host port you mapped). |
| **Login loops** back to the sign-in page after an upgrade | The session cookie carries `Secure` but the browser is on plain `http://`. Set `TATU_SECURE_COOKIES=0` (the template default), recreate, and log in again. |
| **Permission denied** writing `/data` | The bind mount is not owned by uid 10001. Run `chown -R 10001:10001 /mnt/user/appdata/tatutime`. Unraid's *New Permissions* tool can reset ownership to `nobody:users`; re-run the chown after it. |
| **`vault_disabled`** in Settings → Email | The vault is disabled by configuration (`TATU_VAULT_ENABLED=false` or `vault_enabled: false` in `config.json`), or the app could not write `/data/vault.key` because the data directory is not writable by uid 10001. Check the *Vault key* card in Settings → Security: `config`/`env` means it was switched off deliberately; `write_failed` means fix ownership/permissions and restart. |
| **Vault actions disabled / 409 `vault_managed_externally`** | `TATU_VAULT_ENABLED`, `TATU_SECRET_KEY` or `TATU_SECRET_KEY_FILE` is set, so the environment owns the lifecycle. Change or remove that variable (or the external key file) — the Settings enable/disable/regenerate actions only work when the app manages `/data/vault.key`. |
| Container **refuses to start** with "vault store found on disk ... key is missing" | `secrets.bin` exists but no key can open it. Restore the original `vault.key` (or the backup that contains it), or point `TATU_SECRET_KEY_FILE` at the original external key; do not delete `secrets.bin` unless you accept losing the stored credentials. |
| Container **refuses to start** with "TATU_SECRET_KEY is configured but invalid" | An external key is not exactly 32 bytes. Generated keys are 32 base64url characters; for a manual one, `openssl rand -hex 16` prints 32 characters (`-hex 32` prints 64 and is rejected). |
| **Console / `docker exec` fails** | Expected: distroless has no shell. Use `docker logs -f TatuTime`. |
| **Port already in use** | Change the host side of the WebUI port mapping (for example `8081`), keep the container side at `8080`, and update the WebUI URL. |
| Second container on the same appdata exits immediately | The single-instance guard (`.server.lock`) refuses a second server on the same data directory. Run one container per appdata folder. |
| Emails are logged but never sent | No SMTP host is configured, or the host/port is wrong. The app speaks plaintext SMTP only; use the relay from section 6. |

Useful commands (Unraid terminal):

```sh
docker logs --tail 200 TatuTime
docker inspect --format '{{.State.Health.Status}}' TatuTime
docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' TatuTime
```

---

## 11. Environment variables on Unraid

The template sets only the variables that must match its own mapping. Everything
else is optional and can be supplied through the template's **Extra Parameters**
(`-e NAME=value`), `config.json`, or the credential vault. The complete
catalogue, defaults and `_FILE` recipes are in
[`docs/configuration.md`](../../docs/configuration.md).

| Variable | Template value | Why it is in the template |
| --- | --- | --- |
| `TATU_DATA_DIR` | `/data` | Must match the `Data` path target |
| `TATU_PORT` | `8080` | Must match the WebUI port target and the healthcheck |
| `TATU_SECURE_COOKIES` | `0` | Plain-HTTP LAN login; set `1` for HTTPS. This template value overrides the production Secure-cookie default |
| `TATU_SECRET_KEY_FILE` | empty | Optional external vault-key override; empty means the app manages `/data/vault.key` |

Common optional additions:

| Variable | Use |
| --- | --- |
| `TATU_ENV` | Set `production` when serving over HTTPS; it enables the strict session-secret policy. For HTTPS set **both** `TATU_ENV=production` and `TATU_SECURE_COOKIES=1` — the template's `0` overrides the Secure-cookie default |
| `TATU_SESSION_SECRET_FILE` | Mount an external session key instead of the auto-created `/data/session.key` |
| `TATU_VAULT_ENABLED` | `false` disables the vault; any valid value makes the lifecycle operator-managed |
| `TATU_MAX_DOCS` | Per-collection document cap (default 50,000) |
| `TATU_SSO_ALLOWED_DOMAINS` | Comma-separated SSO domains; empty is fail-closed |
| `TATU_SMTP_HOST` / `_PORT` / `_FROM` / `_USERNAME` / `_PASSWORD` | SMTP relay; prefer configuring these in **Settings → Email** so they live in the vault |
| `RUST_LOG` | Tracing filter (default `info`) |

Neither vault key variable is required: the app generates and manages
`<data>/vault.key` on its own. When an external key is wanted, prefer
`TATU_SECRET_KEY_FILE` over `TATU_SECRET_KEY` (and likewise
`TATU_SESSION_SECRET_FILE` over `TATU_SESSION_SECRET`): the value never
appears in the Unraid template XML or `docker inspect`. Setting either (or
`TATU_VAULT_ENABLED`) makes the vault operator-managed: the Settings
enable/disable/regenerate actions are refused with 409
`vault_managed_externally` until the variable is removed.
