# Operations

This page covers running TucanoTime in production: upgrades, backup and restore,
file ownership, container hardening, health and logs, and the background
scheduler. Configuration details live in [Configuration](configuration.md).

## Persistence and upgrades

Everything an installation knows lives in the data directory
(`TUCANO_DATA_DIR`, `/data` in the image). The container itself is disposable;
the volume is the installation. Users, entries, invoices, expenses, settings,
the credential vault, session key and scheduler state all survive a container
recreation.

```sh
docker compose build --pull
docker compose up -d
```

Compose builds the local `tucanotime:latest` image from this repository
(`build: .`), so `docker compose pull` does **not** fetch the published GHCR
image. To run the published image instead, pull it and tag it locally under the
name Compose expects:

```sh
docker pull ghcr.io/tucanotechnology/tucanotime:latest
docker tag ghcr.io/tucanotechnology/tucanotime:latest tucanotime:latest
docker compose up -d
```

Nothing needs to be reconfigured after an image update. A pinned published
image works the same way: pull its `sha-<short>` tag, tag it `tucanotime:latest`
locally and recreate the container. The data layout is summarised in
[Storage and retention](storage-and-retention.md).

## Backup and restore

The image ships an operator CLI on the same binary:

```sh
tucano-time --backup FILE    # archive the data dir to FILE (tar.gz + manifest)
tucano-time --restore FILE   # unpack a backup into the data dir (--force to overwrite)
tucano-time --health         # probe the local server's /healthz
tucano-time --version        # print the version
```

A backup is a plain `tar.gz` with a `manifest.json` recording the crate version
and a sha256 for every file. A restore verifies the checksums before writing. It
refuses a non-empty data directory unless `--force` is given, and it warns when
the archive carries a vault store while no key is configured.

### With the Compose `cli` profile

The `tucanotime-cli` service runs the same image against the same
`tucanotime-data` volume. It is in the optional `cli` profile.

```sh
# back up
docker compose --profile cli run --rm tucanotime-cli \
  --backup /backups/monday.tar.gz

# restore (stop the server first)
docker compose stop tucanotime
docker compose --profile cli run --rm tucanotime-cli \
  --restore /backups/monday.tar.gz
docker compose up -d tucanotime
```

`/backups` maps to `TUCANO_BACKUP_DIR` (default `./backups`) and must be
writable by uid 10001. Back up while the server is stopped, or accept a torn
snapshot across collections.

**Keep the vault key with the backup.** Unless you use an external
`TUCANO_SECRET_KEY[_FILE]`, the generated `<data>/vault.key` is inside the data
directory, so the backup contains both `secrets.bin` and the key that decrypts
it. Protect the archive accordingly, keep a separate copy of `vault.key`, or
supply the key externally and store that copy with the backup. Without the key
the stored credentials are permanently unreadable.

## Bind mounts and file ownership

The image runs as numeric uid/gid `10001:10001` and never changes identity. A
bind-mounted host directory must be writable by that uid:

```sh
mkdir -p ./Data
sudo chown -R 10001:10001 ./Data
```

A named volume is the recommended default and needs no ownership step. On
Unraid, run `chown -R 10001:10001 /mnt/user/appdata/tucanotime` before the first
start; see the [Unraid runbook](../deploy/unraid/README.md).

## Container hardening

The server needs no writable path outside `/data`, so the image and both Compose
services are locked down by default.

| Setting | Effect |
| --- | --- |
| `--read-only` + `--tmpfs /tmp` | Read-only root filesystem; only `/data` (volume) and `/tmp` (tmpfs) accept writes |
| `--cap-drop ALL` | No Linux capabilities |
| `--security-opt no-new-privileges` | No setuid/setgid escalation |
| `user: "10001:10001"` | Numeric, non-root uid/gid fixed in the image and Compose |
| `init: true` | A tiny init reaps orphans and forwards SIGTERM |
| `stop_grace_period: 30s` | The server finishes in-flight writes before exit |
| `ports: 127.0.0.1:8080:8080` | Loopback-only by default; `TUCANO_HOST_ADDR=0.0.0.0` publishes on the LAN |

The runtime base is distroless: there is no shell, no package manager and no
`apt` state. `docker exec … sh` fails by design; use `docker logs`.

## Health and logging

- `GET /healthz` returns the liveness answer plus the build identity:
  `{"status":"ok","version":"0.1.45","build":"5309ec3"}`. The version is the
  crate version in `Cargo.toml` (the release version), and the build is the
  commit the binary was compiled from — seven characters, the same short sha
  release tags use. The GUI footer renders both, and a probe that fails leaves
  the plain `TucanoTime` branding rather than blocking the boot.
- Image builds pass the commit as the `TUCANO_BUILD` build arg
  (`docker build --build-arg TUCANO_BUILD=$(git rev-parse --short HEAD) .`);
  CI does this automatically. The builder context has no `.git`, so without the
  arg the binary reports `unknown`. The image `HEALTHCHECK` runs
  `tucano-time --health`, which probes `http://127.0.0.1:${TUCANO_PORT}/healthz`
  inside the container, so no `curl` or shell is needed.
- `RUST_LOG` controls the tracing filter; the default is `info`.
- `docker logs -f tucanotime` is the primary debugging interface.
- Security-relevant events are appended to `<data>/audit.log` as JSON lines and
  rotated at 1 MiB with one previous generation. See [Security](security.md).

A single-instance guard holds `<data>/.server.lock`; never point two containers
at the same data directory. The second process refuses to start, because two
schedulers would fire every job twice.

## Background scheduler

An in-process scheduler runs the recurring jobs; no external cron is needed.

| Job | Cadence | What it does |
| --- | --- | --- |
| Reminders | daily | Pushes "no time logged today" and Friday "submit your timesheet" notifications |
| Recurring invoices | per schedule | Creates draft invoices from the configured recurring schedules; drafts still need an explicit issue |
| Budget alerts | daily | Checks project budget burn and raises notifications |
| Overdue email reminders | daily | Emails customers about issued, unpaid invoices past their due date, honouring `reminder_days` |

The last-run timestamps are persisted in `<data>/scheduler.json`, so a restart
does not re-fire a job that already ran. Work is idempotent from domain state;
a crash between a run and the state write costs at most one duplicate.

## Published images

CI publishes on every push to `main`:

```
ghcr.io/tucanotechnology/tucanotime:latest
ghcr.io/tucanotechnology/tucanotime:sha-<short-sha>
```

Use the `sha-`-prefixed tag when you want a fixed build. The image is published
at `ghcr.io/tucanotechnology/tucanotime` as `:latest` and `:sha-<short>`; check
the registry package visibility (a public package pulls anonymously without
credentials).

## Image facts

The image is defined by the [`Dockerfile`](../Dockerfile) and run by
[`docker-compose.yml`](../docker-compose.yml).

- Multi-stage build: a Rust `1.98.0` builder and a distroless
  `cc-debian13:nonroot` runtime. Both stages are pinned by digest.
- The GUI and the OpenAPI contract are embedded in the binary; there is no
  runtime build step and no CDN.
- The process runs as uid/gid `10001:10001`, listens on `8080` and exposes the
  data volume at `/data`.
- Dependencies compile in a cached layer, so a source or GUI edit rebuilds only
  this crate.
- The image size is not documented here; measure it on your own build with
  `docker image ls tucanotime` if you need the number.

## Unraid

The canonical Unraid template and step-by-step runbook live in
[`deploy/unraid/README.md`](../deploy/unraid/README.md), with the template at
[`deploy/unraid/my-TucanoTime.xml`](../deploy/unraid/my-TucanoTime.xml). The
Unraid runbook adds platform-specific steps (flash-share import, uid 10001
permissions, the vault key, SMTP relay options and backups); use this page for
generic operations.

---

Last verified against TucanoTime 0.1.41.
