# Deployment

## Image

`ghcr.io/joanfabregat/cannery-row`, built from `Containerfile.rust` by `.github/workflows/rust-release.yml`. One image serves the API, the MCP endpoint (`/mcp`) and the embedded web app, and runs the runner, the stock policy, imports and the migrations. It holds only the static `cannery` binary on a distroless base: no shell, interpreter or package manager. Every pull request builds it and smoke-tests it without pushing ([operations](rust/operations.md#image)).

Pin a deployment to an image digest (`@sha256:…`), not a moving tag. The release workflow also builds the same binary as a standalone archive for Linux (amd64, arm64, static) and macOS (arm64).

## Configuration

The API bounds buffered REST control bodies to 2 MiB and the complete body read to 30 seconds; exceeding either returns a sanitized HTTP 400. JSON bodies may nest at most 127 containers; deeper bodies, like other JSON syntax errors, return HTTP 422. Artifact byte streams and MCP use their separate limits; see [request bodies and JSON](rust/limits.md#request-bodies-and-json).

Settings come from environment variables (`CANNERY_<SECTION>_<FIELD>`) or a TOML file named by `CANNERY_SETTINGS` or `--settings`; see `settings.example.toml` for the common fields. Every section and field is listed in `SECTIONS` in `crates/core/src/settings/parse.rs`. Supply secrets through environment variables only.

| Variable | Required | Meaning |
| --- | --- | --- |
| `CANNERY_DATABASE_URL` | with `url` | PostgreSQL URL. It must be a direct connection, not a transaction-pooling proxy: the sweeps use a session advisory lock. |
| `CANNERY_DATABASE_PROVIDER` | no | `url` (the default) connects to `CANNERY_DATABASE_URL`; `managed` runs a private PostgreSQL; see [Managed database](#managed-database). |
| `CANNERY_DATABASE_DATA_DIR`, `CANNERY_DATABASE_POSTGRES_BIN_DIR`, `CANNERY_DATABASE_POSTGRES_CACHE_DIR` | with `managed` | The data directory, a directory with existing `postgres` and `initdb` binaries (instead of the bundled ones), and where the bundled ones are unpacked. With `url`, the bin directory is only where `cannery db dump` finds `pg_dump`. |
| `CANNERY_SERVER_PUBLIC_BASE_URL` | yes | The public `https://` origin users reach, for example `https://cannery.example.org`; the OIDC callback is `<base>/auth/callback`. |
| `CANNERY_AUTH_OIDC_ISSUER`, `CANNERY_AUTH_OIDC_CLIENT_ID`, `CANNERY_AUTH_OIDC_CLIENT_SECRET` | for web login | The OIDC client; all three or none. Without them only bearer tokens work. |
| `CANNERY_AUTH_BOOTSTRAP_ADMIN_EMAILS` | first install | Comma-separated verified emails that become installation admins on login. |
| `CANNERY_AUTH_ALLOWED_EMAIL_DOMAINS` | no | Comma-separated email domains allowed to log in (empty: any verified email). |
| `CANNERY_STORAGE_BACKEND` | no | `local` (the default) or `s3`; see [Object storage](#object-storage). |
| `CANNERY_STORAGE_LOCAL_ROOT` | preset | The local artifact store. The image sets `/data/objects`: mount a persistent volume at `/data`. |
| `CANNERY_STORAGE_S3_*`, `CANNERY_STORAGE_BUCKET` | with `s3` | The S3-compatible store; see [Object storage](#object-storage). |
| `CANNERY_STORAGE_VALIDATE_JSON_MAX_BYTES` | no | Largest stored job output whose JSON or JSON Lines content the API validates against its interface (64 MiB by default). A larger one is still checked for size and leading bytes, and is recorded as not content-validated. JSON is held in memory up to this size while it is validated, so keep it well under the server's memory per validation. |
| `CANNERY_STORAGE_MAX_CONCURRENT_VALIDATIONS` | no | How many stored job outputs one API process validates at once (2 by default). Validation acquires a slot after upload completion, so a slow receiving client does not occupy one. Each may hold up to `CANNERY_STORAGE_VALIDATE_JSON_MAX_BYTES` plus its parsed form. The permit wait and the readback/checking each have a separate 20-second deadline, returning retryable 503 on expiry. `CANNERY_STORAGE_MAX_STREAM_SECONDS` instead controls the recovery age of receiving uploads. See [uploads and storage](rust/limits.md#uploads-and-storage). |
| `CANNERY_WEB_DIST_DIR` | no | A directory holding another build of the web app (with its `index.html`) to serve instead of the one embedded in the binary. Unset: the embedded build. |
| `FORWARDED_ALLOW_IPS` | behind a proxy | Comma-separated addresses of the reverse proxy whose `X-Forwarded-*` headers are trusted (default `127.0.0.1,::1`). `cannery serve --forwarded-allow-ips` takes precedence. |

The image runs as the non-root UID 10001 and GID 10001. The volume at `/data` must be writable by that UID (for example `chown -R 10001:10001` on the host directory, or `fsGroup: 10001` in Kubernetes).

## Object storage

Artifacts (attempt uploads and job outputs) live in an object store. `CANNERY_STORAGE_BACKEND` chooses it, once per installation: an artifact records the store it was verified in, and an artifact of another store is not found, so switching stores does not carry existing artifacts over.

**`local`** (the default) keeps objects on the API's disk under `CANNERY_STORAGE_LOCAL_ROOT`. Every upload and download streams through the API. It suits development, tests and single-host installs.

**`s3`** keeps them in a bucket of an S3-compatible store (AWS S3, Garage, MinIO, Cloudflare R2). Clients send and fetch the bytes directly to and from the bucket through presigned URLs, and the API only verifies and records them ([the protocol](contracts.md#uploads-and-downloads)). Streaming a file through the API still works for a file at or under the multipart threshold.

| Setting (`CANNERY_STORAGE_…`) | Standard fallback | Meaning |
| --- | --- | --- |
| `BUCKET` | `S3_BUCKET` | The bucket. Give the installation a bucket of its own, or a prefix of its own (`CANNERY_STORAGE_S3_PREFIX`): the sweep aborts stale multipart uploads under the prefix. |
| `S3_ENDPOINT` | `AWS_ENDPOINT_URL_S3`, then `AWS_ENDPOINT_URL` | The endpoint the API calls, for example `https://s3.example.org`; unset for AWS S3. |
| `S3_PUBLIC_ENDPOINT` | | The endpoint presigned URLs name, when clients reach the store under another name than the API (for example the API calls an in-cluster service). A SigV4 signature covers the host, so it must be the name clients use. Unset: `S3_ENDPOINT`. |
| `S3_REGION` | `AWS_REGION`, then `AWS_DEFAULT_REGION` | The signing region (`garage` for Garage). Default `us-east-1`. |
| `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY` | `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` | Required. The key needs read, write and delete on the bucket, and the multipart operations. The API never hands it out, and settings errors never repeat it. |
| `S3_PATH_STYLE` | `S3_FORCE_PATH_STYLE` | `true` for path-style URLs (`https://host/bucket/key`), which Garage and MinIO need. Default `false` (virtual-hosted). |
| `S3_PREFIX` | | A key prefix for every object, `""` or ending in `/`. |
| `S3_PRESIGN_TTL_SECONDS` | | Lifetime of a presigned URL (900 by default), never past its upload grant's expiry. Download redirects use at most 5 minutes. |
| `S3_MULTIPART_THRESHOLD_BYTES` | | Uploads over this size (256 MiB by default) go in parts; others in one PUT. |
| `S3_PART_SIZE_BYTES` | | The part size (64 MiB by default, at least 5 MiB); `CANNERY_STORAGE_MAX_OBJECT_BYTES` must fit in 10,000 parts. |

Precedence, for each field: the `CANNERY_STORAGE_*` variable, then the settings file, then the standard variable, then the default. The standard variables are read only with the `s3` backend, so the `AWS_*` and `S3_*` names that S3 tooling commonly puts in a Secret can configure the store as they are.

**Browser access (CORS).** Clients outside the API's origin talk to the bucket directly. The runner does not need CORS. A browser does when it reads an artifact from a page: downloads are plain links followed through a redirect, which needs no CORS, but a web client that uploads or fetches bytes needs the bucket to allow the API's origin for `GET`, `HEAD` and `PUT`, with the `Content-Length`, `Content-Type` and `x-amz-checksum-sha256` request headers, and `ETag` exposed.

**Garage.** Tested with Garage v2.4.1 (CI runs the storage tests against it). Use a dedicated key and bucket, path style and region `garage`. Garage verifies the `x-amz-checksum-sha256` a presigned single PUT signs, which makes such an URL write only the declared bytes. It ignores `If-None-Match: *` on PUT and `If-Match` on DELETE, so the store checks a key with a HEAD before writing or deleting a given version; the API serializes the requests that work on one upload grant, which closes that window in practice.

**Google Cloud Storage (not tested yet).** GCS speaks the S3 XML API through HMAC keys: create an HMAC key for a service account with access to the bucket, then set `S3_ENDPOINT=https://storage.googleapis.com`, the HMAC key as the access key pair, and a region such as `auto`. GCS does not implement every S3 checksum header and has its own multipart semantics, so check the presigned single PUT (its signed `x-amz-checksum-sha256`) and multipart uploads against it before relying on it.

**Downloads from the bucket.** An artifact download or a job input redirects to a presigned `GET`, valid for at most 5 minutes, which the bucket serves. It lacks the `X-Content-Type-Options: nosniff` and `Content-Security-Policy: sandbox` headers the API sets when it streams a download. The risk is low: the URL still forces an attachment and a passive media type (`response-content-disposition`, `response-content-type`, the same allow-list as a streamed download), and the bucket is another origin than the API, holding none of its cookies or tokens, so a file a browser rendered anyway could not act as the API. Keep it that way: serve the bucket from its own host name, never under the API's origin. A store that sets response headers per bucket (a CDN in front of it) can add both headers back.

**Store failures.** When the object store fails or cannot be reached, an upload request (grant, PUT, presign, finish) or a download answers `503` with the error code `store_unavailable`, and the upload grant stays as it was, so the client can retry; the runner retries such a request 4 times with a doubling pause.

**Switching an installation to `s3`.** Artifacts verified under the local backend are not moved when `CANNERY_STORAGE_BACKEND` changes to `s3`: once the API uses the bucket, their downloads and their use as job inputs answer `not_found`. If the local objects' volume is removed too, their bytes are gone for good. So either migrate the objects and the database rows yourself, keep the volume and stay on `local`, or, for disposable data such as a test installation, reset the database together with the switch.

**Sweeps.** With `s3`, the background sweep also aborts multipart uploads under the prefix older than an upload grant can live, and the multipart upload of every upload grant that expired (after it settled the grant, so no database lock is held across the call), and deletes the objects of expired and failed grants once their presigned URLs expired. It never deletes a key that holds a verified artifact. Each direct grant writes a key of its own, so a PUT that started before its URL expired and lands after the sweep can only leave an unreferenced object under the prefix, never overwrite an artifact.

## Running

The entrypoint is `cannery` and the default command is `serve`: one process on `0.0.0.0:8000`, with the embedded web app served for any GET outside `/api`, `/auth` and `/mcp`. Scale with replicas. `GET /api/health` returns 200 when the database answers and 503 otherwise; use it for readiness. Terminate TLS at the reverse proxy.

```sh
podman run -d -p 8000:8000 -v cannery-data:/data \
  -e CANNERY_DATABASE_URL -e CANNERY_SERVER_PUBLIC_BASE_URL \
  -e CANNERY_AUTH_OIDC_ISSUER -e CANNERY_AUTH_OIDC_CLIENT_ID -e CANNERY_AUTH_OIDC_CLIENT_SECRET \
  ghcr.io/joanfabregat/cannery-row:<tag>
```

## Migrations

With `provider = "url"` the server does not migrate the database on start. Run `migrate` with the same image and settings before starting a new release, for example as a one-off job or a Kubernetes init container:

```sh
podman run --rm -e CANNERY_DATABASE_URL ghcr.io/joanfabregat/cannery-row:<tag> migrate
```

It applies the pending migrations in order, each in its own transaction, and does nothing when the database is current. A session advisory lock serializes concurrent runs, so several replicas starting together are safe. Migrations are append-only; roll back by restoring the database, not by running an older image against a newer schema.

## Managed database

For one machine without a PostgreSQL server, set `[database] provider = "managed"`: `cannery` then runs its own private PostgreSQL, so a single binary is a complete installation. Every command that uses the database (`serve`, `migrate`, `import`, `db`) starts a private PostgreSQL 17 as its child process and stops it with a fast shutdown when it exits; `serve` also applies pending migrations first, since nothing else can reach this database. `database.url` is ignored.

- **Data directory.** `database.data_dir`, by default `$XDG_DATA_HOME/cannery/postgres` (`~/.local/share/cannery/postgres`) on Linux and `~/Library/Application Support/Cannery/postgres` on macOS. It holds `pgdata/` (the cluster, created by `initdb` on first start), `password` (generated, mode 0600), `run/` (the Unix socket, mode 0700; no TCP port is opened), `postgresql.log` and `lock`. One `cannery` uses it at a time; a second one refuses to start. Back it up only while `cannery` is stopped.
- **Binaries.** A `cannery` built with the `bundled-postgres` feature carries a compressed PostgreSQL (about 5 MB) and unpacks it once, after checking its SHA-256, into `database.postgres_cache_dir` (by default `~/.cache/cannery/postgresql/<version>-<digest>` on Linux and `~/Library/Caches/Cannery/postgresql/<version>-<digest>` on macOS). Bundles exist for Linux x86_64, Linux aarch64 and macOS arm64 (11 or later); each `cannery` carries the one for its own platform. Otherwise, or to use other binaries, set `database.postgres_bin_dir` to a directory with `postgres` and `initdb` of PostgreSQL 17 built with `pg_trgm` (for example `/usr/lib/postgresql/17/bin` from the `postgresql-17` package). A data directory initialized by another major version is refused.
- **Requirements.** `cannery` must run as an unprivileged user: PostgreSQL refuses root. On Linux the bundled binaries need glibc 2.34 or later, so they do not run in the static (musl) image; use a glibc base such as `distroless/cc`. The first start runs `initdb`, which needs `/bin/sh`: initialize the data directory once where a shell exists, after which a shell-less image can run the server. The data directory's path must stay short enough for a Unix socket (about 100 bytes).
- **Crashes.** On Linux the kernel stops the server when `cannery` dies. On macOS, which has no such signal, or if that failed, the next start finds the leftover server through `postmaster.pid`, confirms that the process runs in this data directory (`/proc` on Linux, `lsof` on macOS), stops it, and starts a fresh one.
- **Backups.** `cannery db dump <file>` writes a custom-format dump (`pg_dump -Fc`) with the same binaries, replacing `<file>` atomically with a mode-0600 file. Like every managed command it needs the data directory to itself, so stop `cannery serve` first; it refuses otherwise. The bundled `pg_dump` has no zlib, so dumps are uncompressed (`--compress=0`); compress the file afterwards if needed. With `provider = "url"`, `db dump` runs the `pg_dump` in `database.postgres_bin_dir`, or else the first one on `PATH`, against `database.url` (the password reaches it through `PGPASSWORD`, never its command line).
- **Restores.** `cannery db restore <file>` loads a dump into the managed database (`pg_restore --single-transaction --no-owner --no-privileges`), then applies this release's pending migrations; a dump with migrations this release does not know is refused. It refuses a database that already has tables unless given `--replace`, which discards it: dump it first. The dump is restored into a separate database and swapped in only once it is complete and its migrations check out, so a failed restore leaves the current database as it was. The bundled `pg_restore` cannot read a compressed dump, such as one written by a stock `pg_dump -Fc`; write it with `--compress=0`. A managed dump also moves a local history to a server: restore it there with that server's `pg_restore --no-owner`.
- **Major upgrades.** A release whose PostgreSQL major differs from the data directory's refuses to start it. `cannery db upgrade --from-bin-dir <dir>`, with `<dir>` the old release's binaries (for example its unpacked bundle, still under `database.postgres_cache_dir`), dumps with them, moves the data directory to `<data_dir>.pg<old major>.bak` (the dump is kept inside it), initializes a new one, restores, and checks that every table has the same number of rows. The old directory is never deleted: remove it once the new one is verified, or, to go back, remove the new one and rename the old one back.

## Importing a research history

`cannery import` loads a reviewed research history into a project, in one transaction, with the same image and settings as `migrate` (run `migrate` first). Mount the bundle read-only and start with `--dry-run`, which prints the plan and writes nothing; the users the bundle names must have signed in once. The bundle format, the checks and what can and cannot be imported are in [import.md](import.md).

```sh
podman run --rm -v ./bundle:/bundle:ro -e CANNERY_DATABASE_URL \
  ghcr.io/joanfabregat/cannery-row:<tag> import --bundle /bundle --project <slug> --dry-run
```

## Runner

The runner uses the same image with the `runner` command. It refuses a token file that is not a regular file or that grants any permission to group or others, so the file is mode 600 (or 400). The runner does not check the owner, but with those modes only the owner can read the file, so it must be owned by UID 10001, the image's user, as seen inside the container. The data, step and work directories must be readable (work: writable) by that UID too.

With rootful Docker, container UIDs are host UIDs: `chown 10001:10001 verifier.token` on the host is enough. With rootless Podman, container UIDs go through your user namespace, and a file owned by host UID 10001 is not UID 10001 inside the container. Either hand the file to UID 10001 inside the namespace with `podman unshare chown 10001:10001 verifier.token` (and the same for the directories), as the example below assumes, or keep everything owned by your own user and add `--userns=keep-id:uid=10001,gid=10001`, which maps your UID to 10001 inside the container:

```sh
podman run --rm \
  -v ./verifier.token:/run/secrets/verifier.token:ro -v ./policy.json:/etc/cannery/policy.json:ro \
  -v /srv/cannery/data:/data/runner:ro -v /srv/cannery/steps:/steps:ro -v /srv/cannery/work:/work \
  ghcr.io/joanfabregat/cannery-row:<tag> runner --unisolated-local \
  --token-file /run/secrets/verifier.token --policy /etc/cannery/policy.json --api-url https://cannery.example.org --project <slug> \
  --data-root /data/runner --step-root /steps --work-root /work
```

The runner needs a launcher, which decides how steps run ([what each one guarantees](contracts.md#what-each-launcher-guarantees)):

- `--launcher local` (or its alias `--unisolated-local`, as above) runs each step as a local process, unisolated, as the runner's user, from a fresh copy of `--step-root`: use it only for trusted fixture and development code. The image holds only the `cannery` binary, with no shell or interpreter, so local steps (a Python script, for example) need the runner to run where their tools are installed: from a release binary on such a machine, or from an image built `FROM` a base that has them with the binary copied in.
- `--launcher docker` runs each step in its own container, from the image its manifest pins, through the Docker Engine API. `--step-root` is not used. This is the launcher for untrusted step code on a dedicated machine; it is set up as described in the next section.
- `--launcher kubernetes` runs each step in its own Pod, from the image its manifest pins, through the Kubernetes API, the runner itself running in the same namespace. `--step-root` is not used. This is the launcher for untrusted step code in a cluster; it is set up as described in [The runner on Kubernetes](#the-runner-on-kubernetes).

Without one of them, the runner refuses to start (exit code 2). Options of another launcher than the chosen one are ignored, with a warning: the local launcher ignores `--runner-id` and every `--docker-…` and `--k8s-…` option, the Docker launcher the `--k8s-…` ones, and the Kubernetes launcher the `--docker-…` ones (`--docker-gpu-devices` included).

### Job kinds and the configuration file

One runner process can run several **job kinds**, each with its own service-account token. A service account has exactly one kind, so each kind needs its own account and token:

| Kind | Token of a service account of kind | What it does |
| --- | --- | --- |
| `verify` | `verifier`, named like the science revision's `verify.verifier.id` | Claims the verify jobs registered to its account under its policy's revision, runs their steps (producer, validators, scorer) through the launcher, applies the policy (the stock gates, in-process, or a [policy step](contracts.md#policy-steps) through the launcher) and publishes the verification report. |
| `experiment` | `experimenter` | Claims the hypotheses of [`workflow` tracks](contracts.md#workflow-tracks), runs each one's experiment workflow through the launcher and submits the result as an agent would ([the experiment kind](#the-experiment-kind)). |

The flags above run the `verify` kind alone, `--policy` naming its policy file. To run more, or to keep the settings in one place, give a configuration file instead: `cannery runner --config runner.toml` (TOML, or JSON when the name ends in `.json`). It replaces every flag but `--once`; combining them is refused. Relative paths in it resolve against the file's directory. Each key is the flag of the same name without its dashes:

| Key | Flag | Meaning |
| --- | --- | --- |
| `api_url` | `--api-url` | Required. Cannery Row base URL. |
| `project` | `--project` | Required. Project slug. |
| `data_root` | `--data-root` | `datasets/<id>/<revision>/` and `baselines/<id>/<revision>/`. Required by `verify` and `experiment`. |
| `work_root` | `--work-root` | Parent of per-job directories. |
| `cache_root`, `cache_max_bytes` | `--cache-root`, `--cache-max-bytes` | [The code and dependency cache](#the-code-and-dependency-cache), shared by every kind. |
| `[launcher]` `type` | `--launcher` | `docker`, `kubernetes` or `local`. Required: both kinds run steps. |
| `[launcher]` `runner_id` | `--runner-id` | Required by `docker` and `kubernetes`: names and labels this runner's containers or Pods. |
| `[launcher]` `step_root` | `--step-root` | The local launcher's step root. |
| `[launcher]` `docker_host`, `docker_user`, `docker_job_root_host`, `docker_pids_limit`, `docker_tmp_size`, `docker_shm_size`, `docker_gpu_mode`, `docker_gpu_devices` | the same, with dashes | The Docker launcher ([options](#other-options)). `docker_gpu_devices` may also be a list of indices (`[0, 1]`; `[]` for none). `docker_job_root_host` is a path on the Docker host, never resolved against the file. |
| `[launcher]` `k8s_namespace`, `k8s_api_url`, `k8s_token_file`, `k8s_ca_file`, `k8s_storage_class`, `k8s_volume_size`, `k8s_gpu_runtime_class`, `k8s_step_user`, `k8s_scheduling_timeout`, `k8s_transfer_image`, `k8s_tmp_size`, `k8s_shm_size`, `k8s_max_output_bytes`, `k8s_max_output_files`, `k8s_exec_idle_timeout` | the same, with dashes | The Kubernetes launcher ([options](#options)). Timeouts are numbers of seconds, `k8s_max_output_files` an integer; `k8s_token_file` and `k8s_ca_file` resolve against the file's directory. |
| `[github]` `token_file`, `app_id`, `app_key_file`, `app_installation_id`, `api_url`, `allowed_repos` (a list) | `--github-…` | [The runner's GitHub credential](#the-runners-github-credential), shared by every kind. |
| `[[kinds]]` | none | One table per kind to run, below. At least one. |

Each `[[kinds]]` table has:

| Key | Default | Meaning |
| --- | --- | --- |
| `kind` | required | `verify` or `experiment`. |
| `token_file` | required | The kind's own token: a regular file of mode 600 (or 400) holding the token of a service account of the matching kind. The runner checks the mode only, not the owner, so the file must belong to the user the runner runs as ([Runner](#runner)). Two kinds of different types never share a token; two `verify` entries may. |
| `name` | the kind | Labels the entry's log lines (`cannery runner: verify-step: job … completed`); unique. |
| `poll_seconds` | `10` | How long the entry waits when no job is queued or a claim failed. |
| `concurrency` | `1` | How many jobs of this entry run at once, each its own loop. Resource ceilings (`cpu`, `memory`) apply per step, so `n` loops (across every entry) can together ask the host for `n` times a step's ceiling: size the host for the sum, or keep `1`. GPUs are never shared: each GPU step takes its own ([GPUs](#gpus)). |
| `policy` | required by `verify` | `verify` only: the verifier's policy, a stock configuration (`policy.json`) or a policy step file. |

Every kind shares the launcher, the GitHub credential and the cache root, and loops on its own. A kind whose claims fail, or whose jobs crash, is logged and keeps polling; it never stops another. `SIGTERM` (as `podman stop` sends) cancels every running job, kills its step and removes its containers, then exits with code 143. With `--once`, each entry runs at most one job, side by side, prints its outcome (`no job waiting`, `job <id> completed`), and the process exits 1 if any entry's claim or job raised (each entry's outcome is printed first). An invalid file, an unreadable token or a kind that lacks the launcher or data root it needs refuses to start (exit code 2), naming the key (`kinds[1].token_file: …`).

This runs the experiment and verify stages of a project in one process with the Docker launcher, its verifier applying the stock gates:

```toml
# /etc/cannery/runner.toml
api_url = "https://cannery.example.org"
project = "pilchards"
data_root = "/data/runner"
work_root = "/work"

[launcher]
type = "docker"
runner_id = "runner-1"

[github]
app_id = "123456"
app_key_file = "/run/secrets/github-app.pem"
app_installation_id = "7654321"

[[kinds]]
kind = "verify"
token_file = "/run/secrets/verifier.token"
policy = "/etc/cannery/policy.json"

[[kinds]]
kind = "experiment"
token_file = "/run/secrets/experimenter.token"
```

```sh
podman run --rm \
  -v ./runner.toml:/etc/cannery/runner.toml:ro -v ./policy.json:/etc/cannery/policy.json:ro \
  -v ./verifier.token:/run/secrets/verifier.token:ro \
  -v ./experimenter.token:/run/secrets/experimenter.token:ro \
  -v ./github-app.pem:/run/secrets/github-app.pem:ro \
  -v /srv/cannery/data:/data/runner:ro -v /srv/cannery/work:/work \
  -v /var/run/docker.sock:/var/run/docker.sock \
  ghcr.io/joanfabregat/cannery-row:<tag> runner --config /etc/cannery/runner.toml
```

Every file is owned by UID 10001 as the container sees it, as above. On a Container-Optimized OS VM, mount the configuration file and the tokens like the verifier token in [the cloud-config below](#cloud-config) and replace the flags with `--config`. To verify with a policy step instead of the stock gates, point `policy` at a policy step file ([the contracts](contracts.md#policy-steps) describe it; `examples/fixture/policy-step.json` is one): the step runs through the same launcher, after the job's other steps. To switch policy revisions without stopping verification, list two `verify` entries with different `name`s and policies and the same token, then remove the old one once no job of its revision is pending.

### The experiment kind

The `experiment` kind claims the hypotheses of [`workflow` tracks](contracts.md#workflow-tracks) with an **experimenter** service token, runs each one's experiment workflow, and submits the result as an agent would; the verify jobs it leads to are claimed by a `verify` kind (or an agent) as usual. It is configured like any kind, as a `[[kinds]]` entry of the configuration file (there is no flag for it: the flags alone run the `verify` kind), and reads no option of its own:

```toml
[[kinds]]
kind = "experiment"
token_file = "/run/secrets/experimenter.token"
```

It needs a launcher and a data root, as a `verify` kind does, and runs on any of the three launchers with the same container contract, code and setup handling, interface checks and validators: its steps run exactly as a verify job's. Its entry shares the process's launcher, GitHub credential and cache root with the other kind, so a project with workflow tracks usually runs both kinds in one process, as in the example above. `concurrency` runs several experiments at once, each in its own loop.

Create an **experimenter** service account in the project for it (`"kind": "experimenter"`, from the admin settings or `POST /api/projects/{slug}/service-accounts`) and give the runner a token of it. Only an experimenter claims hypotheses of `workflow` tracks, and it claims nothing else; it cannot write plans, comment or decide. Its reports of a run's failure are trusted (the failure is retried automatically, then reviewed), so its token must stay with the runner: never hand it to an agent. An attempt it claims shows that account as the claimant and as the author of its run document. Experiment steps are candidate code, so outside fixtures and development use the Docker launcher ([on a Container-Optimized OS VM](#the-runner-on-a-container-optimized-os-vm)) or the Kubernetes one ([on Kubernetes](#the-runner-on-kubernetes)), as for producers. The experiment runner reads the predecessor attempt's artifacts through the API under the attempt's lease, so it needs no storage credential. A runner of the experiment kind that loses its lease stops the step and leaves the attempt to the sweep, which queues the hypothesis again while the science revision's `max_auto_retries` allows; so does an attempt still running past its deadline. A claim that finds only hypotheses of tracks whose workflow cannot run under the current science revision is logged (`workflow_unavailable`, naming the tracks), and the runner keeps polling.

### The runner's GitHub credential

A step manifest with `code` ([steps as scripts](contracts.md#steps-as-scripts-code-setup-and-the-dependency-cache)) runs a script from a GitHub repository at a pinned commit. The runner fetches that commit itself, through the GitHub REST API, with a read-only credential it keeps in memory: it never reaches a step's environment, files or command line, and it is sent to the API only, never to the short-lived download URL GitHub redirects to. Public repositories need no credential. For private ones, give the runner one of:

- **A GitHub App** (recommended): the runner mints installation tokens from the App's private key (a JWT signed with RS256), and mints a new one five minutes before each expires (they last an hour). Nothing long-lived but the key, which can be rotated in the App's settings.
- **A token file**, `--github-token-file`: a fine-grained personal access token limited to the step repositories with the **Contents: Read-only** permission, or any token minted elsewhere. It does not refresh, so a token that expires stops the runner fetching new commits (`runner_error`).

To create the App (an organization's or your own account's):

1. In GitHub, **Settings → Developer settings → GitHub Apps → New GitHub App**. Name it (say `cannery-runner-<org>`), give any homepage URL, and untick **Webhook → Active**.
2. Under **Repository permissions**, set **Contents** to **Read-only** (Metadata: Read-only is added by itself). Grant nothing else: no write permission, no organization or account permission.
3. Under **Where can this GitHub App be installed?**, keep **Only on this account**, then **Create GitHub App**. Note the **App ID** on the next page.
4. **Private keys → Generate a private key** downloads a `.pem` file: this is the runner's credential. Copy it to the runner's machine, mode 600, owned by the runner's UID (10001 in the image), like the verifier token; then delete the download.
5. **Install App**, choose the account, and **Only select repositories**: the repositories that hold step code. The installation's URL ends with its id (`…/settings/installations/<installation id>`).

Then start the runner with `--github-app-id <App ID> --github-app-key-file <file> --github-app-installation-id <installation id>`. A key that cannot sign refuses to start (exit code 2). An installation that does not cover a step's repository makes GitHub answer as if the repository did not exist, and the job fails with `runner_error`: "GitHub has no commit … or the runner's credential cannot see that repository".

Before it downloads a commit, the runner checks that the commit belongs to the step's repository and not only to one of its forks. GitHub serves a fork's commits through the parent's URLs too. The commit must be reachable from the default branch or from one of the first 100 other branches. The runner checks this with `GET /repos/{repo}`, `/branches` and `/compare/{branch}...{sha}`, which need nothing beyond **Contents: Read-only**, with a token or an App alike. A commit that fails the check is refused like a missing one (`runner_error`). This costs a few API calls once per new commit, never per job.

When GitHub rate-limits the runner (HTTP 403 or 429 with `Retry-After`, or `X-RateLimit-Remaining: 0` with `X-RateLimit-Reset`), the runner waits as asked and retries once, if the wait is at most 60 seconds. A longer wait, or a second limit, fails the job with `runner_error` and says when GitHub will accept requests again. An App installation's limit grows with the organization's repositories; an anonymous runner's (public repositories only) is 60 requests an hour.

The science revision says which repositories steps may run code from (`code_repositories`, per trust class, see [steps as scripts](contracts.md#steps-as-scripts-code-setup-and-the-dependency-cache)). `--github-allowed-repos` adds a second, runner-side allowlist on top. A job whose step names a repository outside it fails with `code_not_allowed`, even when the science revision allows the repository. Use it on a runner shared between projects, or to pin a runner to the repositories its App is installed on.

| Option | Default | Meaning |
| --- | --- | --- |
| `--github-app-id` | none | The App's numeric id. |
| `--github-app-key-file` | none | The App's private key (PEM), mode 600. |
| `--github-app-installation-id` | none | The App's installation on the step repositories. |
| `--github-token-file` | none | A read-only token instead of an App, mode 600. Not with the App options. |
| `--github-api-url` | `https://api.github.com` | The REST API, for GitHub Enterprise Server (`https://<host>/api/v3`). |
| `--github-allowed-repos` | none (what the science revision allows) | `owner/name` repositories steps may run code from, comma-separated or repeated; any case. |

### The code and dependency cache

The runner keeps code trees and setup caches in its **cache root**: `--cache-root`, or `<work root>/cache` by default (without either, steps with `code` fail with `runner_error`). Each commit is extracted once (`code/<owner>/<repo>/<commit>/`) and each setup's output once per cache key (`setup/<key>/`); work in progress lives in `tmp/` and is cleared when the runner starts. The runner takes a lock on the root, so **each runner needs its own** cache root: a second runner on the same one fails its scripted steps with `runner_error`.

The cache is capped by `--cache-max-bytes` (default `20Gi`). After each new entry, the least recently used entries are removed until the rest fits, never one a running step uses; a new setup cache is held from the moment it is published. Size the disk of the cache root for the cap plus the largest setup in progress.

The runner refuses to publish a setup cache that holds a symbolic link resolving outside it, or a directory it cannot read. The setup fails with `setup_failed`, so a Python virtual environment, whose interpreter links into the image, cannot be cached. Code trees get the same check when they are extracted.

With the Docker launcher, the daemon bind-mounts entries, and a setup's key files, into containers, so it must see the cache root under the same path as the runner. With `--docker-job-root-host`, keep the cache root under `--work-root` (the default does), where the same translation applies. The runner refuses to start (exit code 2) when `--cache-root` is outside it. With the Kubernetes launcher, the runner copies entries into each step's directory on the per-job volume (see [The runner on Kubernetes](#the-runner-on-kubernetes)), so the cache root can be anywhere in the runner's Pod; give it a volume of its own that outlives the Pod if caches should survive a restart.

| Option | Default | Meaning |
| --- | --- | --- |
| `--cache-root` | `<work root>/cache` | Code trees and setup caches, one runner per root. |
| `--cache-max-bytes` | `20Gi` | Size cap of the cache root, least recently used entries evicted first. |

## The runner on a Container-Optimized OS VM

This section sets up `cannery runner --launcher docker` on a Google Compute Engine VM running Container-Optimized OS (COS), the runner itself in a container started by systemd. Any Linux machine with Docker Engine 20.10 or later works the same way; COS is the documented case.

### The isolation boundary

The runner starts step containers through the Docker socket, `/var/run/docker.sock`, mounted into its own container. **Whoever can use that socket is root on the machine**: it can start a privileged container that mounts the host. The runner is trusted code, but that makes the VM, not the runner's container, the isolation boundary between steps and everything else. So:

- Dedicate the VM to one runner. Run nothing else on it, and do not share its Docker daemon with anything else.
- Give it only what the runner needs: the verifier token, its policy file, the datasets and baselines of the project it verifies, and a work directory. Nothing on it should be worth more than what the verifier token can reach.
- Create it without a service account (`--no-service-account --no-scopes`), so a step that reaches the metadata server finds no credentials. Steps with `network: none` reach nothing; a step that declares egress gets Docker's bridge network, where **the egress allowlist is not enforced** and any destination answers, so the unit below also drops traffic from containers to the metadata server (`169.254.169.254`).

Each step container is still locked down: the runner's own non-root user (UID and GID 10001 in the image), a read-only root filesystem, no capabilities, `no-new-privileges`, a PID limit, memory and CPU limits from its manifest, and only its own job directory mounted (inputs and `job.json` read-only, outputs writable).

### Files on the VM

COS keeps `/var` across reboots and resets `/etc` at each boot (the cloud-config below writes the unit again each time). Everything the runner keeps lives under `/var/lib/cannery`, owned by UID and GID 10001, the image's user:

| Path | Mode | Holds |
| --- | --- | --- |
| `/var/lib/cannery/verifier.token` | `600` | The verifier service token (see [Runner](#runner) for the token file's rules). |
| `/var/lib/cannery/policy.json` | `644` | The verifier's policy file, a stock configuration or a policy step file ([the contracts](contracts.md#verification-policy-and-the-stock-policy)). |
| `/var/lib/cannery/data/` | `755` | `datasets/<id>/<revision>/` and `baselines/<id>/<revision>/`, as for any runner. |
| `/var/lib/cannery/work/` | `700` | Per-job directories, created and removed by the runner, and `cache/`, the [code and dependency cache](#the-code-and-dependency-cache). |
| `/var/lib/cannery/github-app.pem` | `600` | Optional: the [GitHub App's private key](#the-runners-github-credential), for steps whose code is in private repositories. Copy it like the token. |

Never put the token in the VM's metadata (`user-data` included): anything on the VM that reaches the metadata server can read it. Copy it over SSH once; it stays on the persistent disk:

```sh
gcloud compute scp verifier.token cannery-runner-1:~/verifier.token --zone <zone>
gcloud compute ssh cannery-runner-1 --zone <zone> -- \
  'sudo install -D -m 600 -o 10001 -g 10001 ~/verifier.token /var/lib/cannery/verifier.token && rm ~/verifier.token'
```

Copy the data the same way (or with `gcloud storage cp` from a machine that has access), then `sudo chown -R 10001:10001 /var/lib/cannery/data`.

### The work directory and the path-mapping option

The Docker launcher gives each step its job directory as bind mounts. Bind mounts are resolved by the Docker daemon on the VM, not inside the runner's container, so the daemon must find the job directory under the path the runner gives it. The simplest way, used below, is to mount the host's work directory into the runner's container **under the same path** (`-v /var/lib/cannery/work:/var/lib/cannery/work` and `--work-root /var/lib/cannery/work`). If the runner must see it elsewhere (for example `-v /var/lib/cannery/work:/work --work-root /work`), tell it where the host has it with `--docker-job-root-host /var/lib/cannery/work`; the runner then translates each job directory's path under `--work-root` to the same path under that directory.

Steps run as the runner's own UID and GID (10001:10001 in the image), and the runner refuses to start (exit code 2) with a `--docker-user` that differs or when it runs as root: the runner stages the inputs and creates the output directories the step writes, and must read and remove what the step writes there. `/cr/outputs` has no disk quota: a step can fill the disk of the work root, so give the work root a disk (or partition) of its own if that matters.

### cloud-config

Save this as `cloud-init.yaml`, replacing `<tag>`, the API URL and the project slug. COS applies it at every boot.

```yaml
#cloud-config
write_files:
  - path: /etc/systemd/system/cannery-runner.service
    permissions: "0644"
    owner: root
    content: |
      [Unit]
      Description=Cannery Row runner (Docker launcher)
      Wants=network-online.target
      After=network-online.target docker.service
      Requires=docker.service

      [Service]
      Environment=IMAGE=ghcr.io/joanfabregat/cannery-row:<tag>
      ExecStartPre=/bin/mkdir -p /var/lib/cannery/work /var/lib/cannery/data
      ExecStartPre=/bin/chown 10001:10001 /var/lib/cannery/work
      ExecStartPre=/bin/chmod 700 /var/lib/cannery/work
      # Containers (steps that declare egress) never reach the metadata server.
      ExecStartPre=/bin/sh -c 'iptables -C DOCKER-USER -d 169.254.169.254/32 -j DROP 2>/dev/null || iptables -I DOCKER-USER -d 169.254.169.254/32 -j DROP'
      ExecStartPre=-/usr/bin/docker rm -f cannery-runner
      ExecStartPre=/usr/bin/docker pull ${IMAGE}
      # The runner's group gets the socket's group, whatever it is on this image.
      ExecStart=/bin/sh -c 'exec /usr/bin/docker run --rm --name cannery-runner \
        --user 10001:10001 --group-add "$$(stat -c %%g /var/run/docker.sock)" \
        --init --read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges \
        -v /var/run/docker.sock:/var/run/docker.sock \
        -v /var/lib/cannery/verifier.token:/run/secrets/verifier.token:ro \
        -v /var/lib/cannery/policy.json:/etc/cannery/policy.json:ro \
        -v /var/lib/cannery/data:/var/lib/cannery/data:ro \
        -v /var/lib/cannery/work:/var/lib/cannery/work \
        ${IMAGE} runner --launcher docker --runner-id %H \
        --token-file /run/secrets/verifier.token --policy /etc/cannery/policy.json \
        --api-url https://cannery.example.org --project <slug> \
        --data-root /var/lib/cannery/data --work-root /var/lib/cannery/work'
      ExecStop=/usr/bin/docker stop -t 60 cannery-runner
      Restart=always
      RestartSec=10

      [Install]
      WantedBy=multi-user.target

runcmd:
  - systemctl daemon-reload
  - systemctl start cannery-runner.service
```

For steps whose code is in private repositories, mount the GitHub App's key next to the token and name the App: add `-v /var/lib/cannery/github-app.pem:/run/secrets/github-app.pem:ro` to the `docker run` options and `--github-app-id <App ID> --github-app-installation-id <installation id> --github-app-key-file /run/secrets/github-app.pem` to the runner's. The cache root defaults to `/var/lib/cannery/work/cache`, which the daemon sees under the same path. Setups reach their package registries through the `bridge` network, like steps that declare egress, so the metadata server rule above covers them too. The runner refuses steps and setups with network unless it is started with `--allow-unrestricted-egress`, which acknowledges that the egress allowlist is not enforced: add it to the runner's options if any step or setup declares network.

Then create the VM (add the GPU flags below for GPU steps):

```sh
gcloud compute instances create cannery-runner-1 --zone <zone> \
  --machine-type <type> --boot-disk-size 200GB \
  --image-family cos-stable --image-project cos-cloud \
  --no-service-account --no-scopes --shielded-secure-boot \
  --metadata-from-file user-data=cloud-init.yaml
```

Size the boot disk for the images steps pull (they are kept, so the next run starts at once), the data and the largest job's work directory. Prune images that are no longer used from time to time (`docker image prune -a` over SSH).

`--runner-id %H` names the runner after the VM; `--launcher docker` requires it. The runner labels every step container with it (`cannery.runner`, with `cannery.job` and `cannery.step`), names them `cr-<runner id>-<job id>-<step>`, and at start removes any container of the same runner id that a crash or a reboot left behind. Give every runner sharing a Docker daemon its own id; with one runner per VM, the VM's name is enough. Keep it stable across restarts (not the container's host name, which changes at every start), or leftovers are never removed.

`systemctl stop` (and `docker stop`) sends the runner SIGTERM: it cancels the job it runs, removes its step containers and work directory, and exits with code 143; the job is not reported and runs again once its lease expires. `--init` makes the signal reach the runner. `docker stop -t 60` gives it a minute before SIGKILL; whatever a SIGKILL leaves is removed at the next start.

`--once` runs one job and exits; leave it out for a service. Check the service with `sudo journalctl -u cannery-runner -f` and the step containers with `docker ps -a --filter label=cannery.runner`.

### Images and registries

A step's image must be pinned by digest (`registry/name@sha256:…`); the launcher refuses a tag. It pulls an image the VM does not have, without registry credentials, so only public images can be pulled. For a private image, pull it on the VM first (`docker pull` with your credentials over SSH, or `docker-credential-gcr configure-docker` and a pull, for Artifact Registry): the launcher uses an image that is already there.

### GPUs

A step that sets `resources.limits["nvidia.com/gpu"]` gets that many GPUs. COS has no NVIDIA Container Toolkit, so the runner passes the devices itself with `--docker-gpu-mode cos`:

1. Create the VM with a GPU: add `--accelerator type=nvidia-l4,count=1 --maintenance-policy TERMINATE` (and a machine type that takes it, such as `g2-standard-8`).
2. Install the driver at every boot, before the runner starts: COS installs it under `/var/lib/nvidia`, which must then be made executable. Put these lines at the top of `runcmd`:

   ```yaml
   runcmd:
     - cos-extensions install gpu
     - mount --bind /var/lib/nvidia /var/lib/nvidia
     - mount -o remount,exec /var/lib/nvidia
     - systemctl daemon-reload
     - systemctl start cannery-runner.service
   ```

   `/var/lib/nvidia/bin/nvidia-smi` over SSH shows the GPU once it is installed.
3. Add `--docker-gpu-mode cos --docker-gpu-devices 0` to the runner's command line (`0,1` for two GPUs, and so on). The runner runs in a container that does not see `/dev/nvidia<n>`, so it must be told which GPUs it lends.

For a step asking for `n` GPUs, the runner then passes the `/dev/nvidia<i>` of the `n` GPUs it lends that step, `/dev/nvidiactl` and `/dev/nvidia-uvm` into its container, and mounts `/var/lib/nvidia/lib64` and `/var/lib/nvidia/bin` read-only at `/usr/local/nvidia/lib64` and `/usr/local/nvidia/bin`. CUDA base images (`nvidia/cuda`) already look there (`LD_LIBRARY_PATH` and `PATH`); another image must set them.

On a machine with the NVIDIA Container Toolkit installed (most other distributions), keep the default `--docker-gpu-mode nvidia`: the runner then asks Docker for the `n` GPUs it lends by index (`DeviceIDs`), as `docker run --gpus '"device=0,1"'` does.

**GPUs are lent, never shared.** The runner keeps one pool of GPU indices per process: `--docker-gpu-devices` (`0,1`); without it the runner lends no GPU, and a step that asks for one fails. Each step asking for GPUs takes that many distinct ones and gives them back when its container is removed, so steps running side by side (several kinds, or `concurrency` above 1) never get the same GPU. A step that finds too few free waits for them before it starts: the wait is not counted in its own deadline, which starts with the step, but the job's lease and deadline still bound it. A step asking for more GPUs than the pool holds fails at once with `runner_error`. CPU and memory are not pooled: their ceilings apply per step ([concurrency](#job-kinds-and-the-configuration-file)).

### Other options

| Option | Default | Meaning |
| --- | --- | --- |
| `--docker-host` | `unix:///var/run/docker.sock` | The Engine API socket (only `unix://` sockets). |
| `--docker-user` | the runner's own | `uid:gid` steps run as; anything but the runner's own is refused, and so is a runner running as root. |
| `--docker-pids-limit` | `4096` | Processes and threads per step. |
| `--docker-tmp-size` | `1Gi` | The size of each step's `/tmp` (a tmpfs, so it counts against the step's memory limit). With `/dev/shm`, it is the only writable path besides `/cr/outputs`; steps get `HOME=/tmp` and `TMPDIR=/tmp`. |
| `--docker-shm-size` | `64Mi` | The size of each step's `/dev/shm` (shared memory, writable; PyTorch data loaders often need more). |
| `--docker-gpu-mode` | `nvidia` | `cos` on Container-Optimized OS (above). |
| `--docker-gpu-devices` | none | The GPU indices the runner lends to steps, comma-separated ([GPUs](#gpus)). |
| `--docker-job-root-host` | none | The work root as the Docker host sees it (above). |
| `--runner-id` | required | Names and labels this runner's containers (above). |

A manifest's `memory` limit is applied without swap and its `cpu` limit as a CPU quota; a limit the manifest does not declare is not applied, so set limits in every manifest. The kernel kills a step before the runner when memory runs out (`OomScoreAdj`), and the runner reports it as `step_failed`, "ran out of memory". Docker keeps at most the latest 200 MiB of a step's log.

## The runner on Kubernetes

This section sets up `cannery runner --launcher kubernetes`: the runner runs in a Pod, and runs each step in a Pod of its own, in the same namespace. The manifests are in [`deploy/runner-k8s/`](../deploy/runner-k8s/README.md) (kustomize), whose README lists the deployment steps. It needs Kubernetes 1.30 or later (k3s, GKE and others), a StorageClass, and a network plugin that enforces NetworkPolicy.

### How a step runs

The runner requires an explicit Kubernetes API URL, namespace, private bearer-token file and cluster CA file; it does not infer in-cluster credentials. The supplied manifests copy a projected bound ServiceAccount token atomically into a mode 0600 memory-backed file before startup, then refresh it every 15 seconds using a separate pinned utility sidecar. Only those utilities mount the projected identity; the runner reads the private copy, and step Pods receive neither credential. The distroless image has no shell. See [the template's credential and isolation contract](../deploy/runner-k8s/README.md). For each job:

1. On the job's first step, it creates a PersistentVolumeClaim (`ReadWriteOnce`, `--k8s-volume-size` in `--k8s-storage-class`), labelled with the runner and the job. Each step run gets its own directory on it, removed once the step's outputs are copied out. A claim of the same name left by an earlier run, or still terminating, is deleted, and created afresh once gone.
2. Before a step, it packs the step's `job.json`, `inputs/` and empty `outputs/`, and for a step with `code` its code tree under `code/` and its dependency cache under `cache/` (for a setup run, only the key files and an empty cache), into a tar file in its work directory. Links are packed as links, never followed, and a mount source that is itself a link is refused. It starts a *transfer Pod* (`--k8s-transfer-image`, a pinned busybox by default) that mounts the volume, streams the tar into `tar -x` over the Pod exec API (a WebSocket, `v5.channel.k8s.io`), then deletes the transfer Pod and waits until it is gone.
3. It creates the step's Pod and watches its phase. The step's deadline starts when the Pod is Running, so scheduling and the image pull are not counted; a Pod still Pending after `--k8s-scheduling-timeout` (900 seconds by default) is a `runner_error`. The step's log is followed from the Pod log API while it runs.
4. When the step ends on its own, the runner deletes its Pod, waits until it is gone, and copies `outputs/` back with a new transfer Pod, and `cache/` too after a setup run (`tar -c`, then the step's directory is removed from the volume). The runner unpacks it below its job directory's `outputs/` and the setup's cache directory only: links come back as links, verbatim, never followed, and the runner then refuses output links, and cache links that resolve outside the cache, as with any launcher; a hard link is recreated as a hard link to a file the same copy wrote into the same directory, never through a name replaced since, so it adds no data. Outputs (with a setup's cache) larger than `--k8s-max-output-bytes` (the volume's size by default) as a tar or as bytes written, with a name too long for the runner's filesystem, that `tar -c` cannot pack, or with more than `--k8s-max-output-files` files, directories and links (100000), are the step's fault: the job fails with `invalid_output`, or `setup_failed` for a setup. A copy in either direction that moves no byte for `--k8s-exec-idle-timeout` (300 seconds), or a copy-out that takes longer than the step's deadline plus the scheduling timeout, is a `runner_error`.
5. When the job ends, however it ends, it deletes the job's Pods and volume.

Why a transfer Pod: the runner's job directory lives in the runner's Pod, so the data must be copied to where the step's Pod can mount it. Pulling inputs from inside the step's Pod would need network and a credential there, and steps never see credentials. Mounting the volume in the runner's Pod as well would need a `ReadWriteMany` class, or both Pods on one node with a local volume, which is not portable. A transfer Pod mounts the volume between steps only, has no network and no credential, and the copy goes through the API server and the kubelet, which the runner already trusts. The cost is two short Pods per step (the transfer image is small and stays on the node) and the runner's work directory holding a step's inputs twice while they are copied.

A step whose job's lease is lost, or that passes its deadline, is deleted with a grace period of 2 seconds, and the runner waits until the API no longer has the Pod (the kubelet has then killed its processes) before it goes on. A Pod the cluster takes away is a `runner_error`, whatever its container's exit code: a Pod reason `Evicted`, `Preempting`, `Shutdown`, `NodeLost` or `UnexpectedAdmissionError`, a `DisruptionTarget` condition, or a container ended as `ContainerStatusUnknown`. Every Pod is deleted on every path, and at start the runner deletes every Pod (with a `cannery.role`) and volume labelled with its `--runner-id`, which a crash left behind. The step Pod also carries `activeDeadlineSeconds` (its deadline, plus the scheduling timeout and a minute), so it ends even if the runner dies while it runs.

### The namespace and the isolation boundary

Give the runner a namespace of its own (`namespace.yaml`), with nothing else in it: its ServiceAccount can create and delete Pods and volumes there. The namespace enforces Pod Security Admission's `restricted` profile, which the runner, step and transfer Pods all meet, so even a compromised runner cannot start a privileged Pod.

A step's Pod runs as `--k8s-step-user` (10001:10001 by default, never root, with `fsGroup` its group), with a read-only root filesystem, every capability dropped, no privilege escalation and the `RuntimeDefault` seccomp profile. Its environment is the manifest's, which may not set `NVIDIA_*` variables (see GPUs). It runs as the namespace's `default` ServiceAccount with no token mounted and no service links. Do not bind roles, or a cloud identity (GKE Workload Identity), to that `default` ServiceAccount. `/cr` is its directory on the job's volume, read-only, with `/cr/outputs` writable over it, `/cr/code` read-only and `/cr/cache` read-only (writable for a setup run), and the working directory `/cr/code` for a step with `code`; `/tmp` and `/dev/shm` are memory volumes (`--k8s-tmp-size`, `--k8s-shm-size`), counted in its memory. The Pod is labelled `cannery.runner`, `cannery.job`, `cannery.step`, `cannery.role` (`step`, or `transfer` for a transfer Pod) and `cannery.network`. The runner's own Pod must never carry a `cannery.role` label. A setup run gets the same Pod: no token, no environment, volume or `envFrom` beyond the manifest's and `/cr`.

### RBAC

`rbac.yaml` binds the `cannery-runner` ServiceAccount to a Role in its namespace only:

| Resource | Verbs | For |
| --- | --- | --- |
| `pods` | `create`, `get`, `list`, `delete` | Step and transfer Pods: create them, watch their phase, delete them and the leftovers of a crash. |
| `pods/log` | `get` | The step's log. |
| `pods/exec` | `get`, `create` | `tar` in transfer Pods. Exec over a WebSocket is a GET, authorized as `get`, and since Kubernetes 1.30 also as `create`. |
| `persistentvolumeclaims` | `create`, `get`, `list`, `delete` | The per-job volume, and the leftovers of a crash; `get` waits until a terminating one is gone. |

No other permission is needed: no Secrets, no cluster-scoped objects.

### The verifier token

The runner reads its verifier token from a file only its owner can read. Secret files are owned by root, and with `fsGroup` they become group-readable, which the runner refuses, so `runner.yaml` mounts the `cannery-runner-token` Secret only in an init container, which copies it, owner-only, to a memory volume that the runner reads. Create the Secret from a file (`kubectl create secret generic cannery-runner-token --from-file=token=./verifier.token`).

### Storage

The per-job volume uses `--k8s-storage-class`, or the cluster's default class. `ReadWriteOnce` is enough: the transfer Pods and the step's Pod mount it one after the other. Size it (`--k8s-volume-size`, 10Gi by default) for one step at a time: the largest step's inputs, code tree, dependency cache and outputs together (a setup run's key files and the cache it fills), since each step's directory is removed once its outputs are copied out. On k3s, the default `local-path` class works but does not enforce the size: a step can fill the node's disk. On GKE, Persistent Disk classes (`standard-rwo`) have a minimum size (10 GiB for `pd-balanced`). Volumes are deleted when their job ends; with a `Retain` reclaim policy, the PersistentVolumes stay, so prefer a class that deletes.

On a cluster of several nodes, the volume binds where its first Pod runs, which is the job's first transfer Pod, and that Pod asks for no GPU. A node-local class (k3s `local-path`) then pins the PersistentVolume, and so every later Pod of the job, to that node: a GPU step stays Pending until the scheduling timeout if that node has no free GPU. Block storage that is `ReadWriteOnce` (Persistent Disk, EBS, Longhorn) can follow the Pods, but a Pod on another node waits for the volume to detach from the previous one (`Multi-Attach error` events), which counts against the scheduling timeout. Until the launcher pins a job's Pods itself, keep step Pods on one node, or on nodes that all have the GPUs: give the namespace a default node selector (the `scheduler.alpha.kubernetes.io/node-selector` annotation with the PodNodeSelector admission plugin), or use one-node clusters for the runner. A later option could pin every Pod of a job to the node of its first transfer Pod, or give the first transfer Pod the job's GPU affinity.

The runner's own work directory (`/work`, an `emptyDir` of 50Gi in `runner.yaml`) holds each job's directory and a step's tar file while it is copied: size it for the largest step's inputs twice. The data root (`cannery-runner-data`, a PersistentVolumeClaim you create, mounted read-only) holds `datasets/<id>/<revision>/` and `baselines/<id>/<revision>/` as for any runner.

### GPUs

A step that sets `resources.limits["nvidia.com/gpu"]` gets that many GPUs as a Pod limit, so the cluster needs the NVIDIA device plugin (GKE installs it on GPU node pools; on k3s, the NVIDIA Container Toolkit on the node plus the `nvidia-device-plugin` DaemonSet). On k3s the containerd runtime that exposes GPUs is the `nvidia` RuntimeClass: add `--k8s-gpu-runtime-class nvidia`. The runtime class is set on steps that ask for GPUs only: a CUDA image under the `nvidia` runtime would otherwise see every GPU of the node. On GKE, leave it unset; GPU node pools are tainted, and GKE adds the matching toleration to Pods that ask for GPUs. A GPU step waits in Pending while the GPUs are busy, up to the scheduling timeout: raise `--k8s-scheduling-timeout` if GPUs are shared.

The NVIDIA runtime also reads `NVIDIA_VISIBLE_DEVICES` and the other `NVIDIA_*` variables from a container's environment, so a step could otherwise ask for GPUs it was not given. Registration refuses step manifests that set an `NVIDIA_*` variable, and the launcher refuses them again. On the cluster, configure the device plugin so the environment is not trusted: `DEVICE_LIST_STRATEGY=volume-mounts` on the `nvidia-device-plugin` DaemonSet, and `accept-nvidia-visible-devices-envvar-when-unprivileged = false` (with `accept-nvidia-visible-devices-as-volume-mounts = true`) in the NVIDIA Container Toolkit's `config.toml` on each GPU node. The GPU Operator sets both with `devicePlugin.env` and `toolkit.env`.

### Network

`networkpolicy.yaml` holds two policies. `cannery-steps-deny-all` denies all traffic, in and out, DNS included, to every Pod with a `cannery.role` label unless it is labelled `cannery.network=egress`, which the launcher sets on steps whose manifest declares egress, and on setup runs with network (all but `setup.network: none`). `cannery-steps-egress` lets those Pods out to any address except the cloud metadata server (`169.254.169.254`) and the cluster's Pod and Service ranges, plus DNS to the cluster's `kube-dns` Pods, and lets nothing in. Within that, the egress allowlist itself is not enforced yet, as with the Docker launcher: such a step reaches any outside destination, and the nodes' own addresses and your LAN unless you add them to the `except` list. Exec and the Pod log go through the kubelet, not the Pod network, so the policies do not affect transfers or logs.

The Pod and Service ranges are k3s's defaults (`10.42.0.0/16`, `10.43.0.0/16`). Set yours in `kustomization.yaml`, in the `cannery-network` ConfigMap (`POD_CIDR`, `SERVICE_CIDR`), which kustomize copies into the policy and does not deploy. On GKE, read them with `gcloud container clusters describe <cluster> --location <location> --format='value(clusterIpv4Cidr,servicesIpv4Cidr)'` (with VPC-native clusters, the Pod range is the subnet's secondary range, often a `/14` such as `10.8.0.0/14`, and recent clusters take Services from `34.118.224.0/20`). With NodeLocal DNSCache, DNS goes to `169.254.20.10` on the node, which the first rule allows.

A NetworkPolicy does nothing without a plugin that enforces it. k3s enforces it with its embedded kube-router unless started with `--disable-network-policy`; GKE needs Dataplane V2 or network policy enforcement enabled on the cluster. Check it once: run a Pod labelled `cannery.role: step` and `cannery.network: none` in the namespace that waits 15 seconds (see below), then tries `wget -T 5 http://1.1.1.1/` and a DNS lookup, and one labelled `cannery.network: egress` that does the same: the first must fail both, the second succeed both.

Policies apply to a new Pod only once the plugin has seen it: kube-router (k3s) lets a Pod's traffic through for its first few seconds. A step without network therefore starts behind an init container, `network-gate` (the transfer image), which waits until the API server's Service no longer answers from inside the Pod; if it still answers after 120 seconds, the step is a `runner_error` (a missing or unenforced policy). Steps with network have no gate, so for their first seconds they may reach the cluster's Pods and Services despite `cannery-steps-egress`.

### Sizing

| What | Default | Size it for |
| --- | --- | --- |
| The runner's Pod | 250m CPU, 512Mi requested, 2Gi limit | Staging and hashing inputs and outputs; it runs one job at a time. |
| `/work` | 50Gi `emptyDir` | A job's inputs and outputs, the cache root (`<work root>/cache` by default, up to `--cache-max-bytes`), plus a step's inputs and code (or outputs and cache) again while copied. |
| Per-job volume | 10Gi | The largest step's inputs, code, cache and outputs together: a step's directory is removed once its outputs are copied out. |
| Transfer Pods | 50m CPU, 32Mi requested; 1 CPU, 256Mi limit | Fixed. |
| Step Pods | The manifest's limits, as requests and limits | Set `cpu` and `memory` limits in every manifest: undeclared means none. |

The kubelet's log rotation (`containerLogMaxSize`, 10 MiB by default) does not cut the log, which the runner follows as it is written; the runner keeps the first 200 MiB. The kubelet's `podPidsLimit` is the step's PID limit.

### Images

A step's image must be pinned by digest; the launcher refuses a tag. The kubelet pulls it (`imagePullPolicy: IfNotPresent`), with the node's credentials only: the launcher sets no `imagePullSecrets`, so use public images, or a registry the nodes can pull from (on GKE, Artifact Registry in the project of the node pool's service account). An image that cannot be pulled (`ErrImagePull`, `ImagePullBackOff`) is a `runner_error` at once.

### With a configuration file

`runner.yaml` runs the `verify` kind alone, with flags. To run the `experiment` kind in the same Pod as well, or several `verify` entries ([job kinds](#job-kinds-and-the-configuration-file)), give the runner a configuration file instead: every flag below is a `[launcher]` key of the same name without its dashes (`k8s_namespace`, `k8s_volume_size`…), and a policy step or an experiment step runs on the same Kubernetes launcher as the verify job's other steps, in Pods of its own, with the same isolation. The file is not expanded, so it holds the values `runner.yaml` reads from the `cannery-runner` ConfigMap:

```toml
# /etc/cannery/runner.toml
api_url = "https://cannery.example.org"
project = "example"
data_root = "/data/runner"
work_root = "/work"

[launcher]
type = "kubernetes"
runner_id = "k8s-1"
k8s_namespace = "REPLACE_WITH_RUNNER_NAMESPACE"
k8s_api_url = "https://kubernetes.default.svc"
k8s_token_file = "/run/cannery/kubernetes.token"
k8s_ca_file = "/run/cannery/kubernetes-ca.crt"
k8s_volume_size = "20Gi"

[[kinds]]
kind = "verify"
token_file = "/run/cannery/verifier.token"
policy = "/etc/cannery/policy-step.json"

[[kinds]]
kind = "experiment"
token_file = "/run/cannery/experimenter.token"
```

To deploy it, put `runner.toml` and the policy file in a ConfigMap mounted read-only at `/etc/cannery`, replace the runner container's `args` with `runner`, `--config`, `/etc/cannery/runner.toml`, `--k8s-namespace-policy-acknowledged`, and extend both credential utilities to copy the experimenter token owner-only beside the verifier token. Config-file launcher settings replace corresponding flags; the operator isolation-policy acknowledgement remains explicit. Each credential is a separate Secret: the verifier account must match the science revision's `verify.verifier.id`, and the experimenter account has kind `experimenter`. Restart the runner after rotating its Cannery credentials; the Kubernetes token file is reread dynamically. Each kind's `concurrency` loops run steps side by side: size per-job volumes and `/work` accordingly. Networked steps additionally require the deliberate `--allow-unrestricted-egress` flag.

### Options

| Option | Default | Meaning |
| --- | --- | --- |
| `--runner-id` | required | Labels and names this runner's Pods and volumes; leftovers of the same id are deleted at start. Unique per namespace, stable across restarts. |
| `--k8s-namespace` | required | Dedicated namespace for step Pods and volumes. |
| `--k8s-storage-class` | the cluster's default | The per-job volume's class. |
| `--k8s-volume-size` | `10Gi` | The per-job volume's size. |
| `--k8s-gpu-runtime-class` | none | `runtimeClassName` of steps that ask for GPUs (`nvidia` on k3s). |
| `--k8s-step-user` | `10001:10001` | `uid:gid` of step and transfer Pods; never 0. |
| `--k8s-scheduling-timeout` | `900` | Seconds a Pod may stay Pending (scheduling and image pull). |
| `--k8s-transfer-image` | busybox 1.37.0 by digest | The transfer Pods' image: it needs `sh`, `tar` and `chmod`, pinned by digest. |
| `--k8s-tmp-size` | `1Gi` | Each step's `/tmp`, in memory. |
| `--k8s-shm-size` | `64Mi` | Each step's `/dev/shm`, in memory. |
| `--k8s-max-output-bytes` | the volume size | The most a step's outputs may weigh, as a tar; more fails the job with `invalid_output`. |
| `--k8s-max-output-files` | `100000` | The most files, directories and links in a step's outputs; more fails the job with `invalid_output`. |
| `--k8s-exec-idle-timeout` | `300` | Seconds a copy to or from the volume may go without a byte moving; then it is a `runner_error`. |
| `--k8s-api-url`, `--k8s-token-file`, `--k8s-ca-file` | explicit configuration | API address and owner-only bearer file are required, including inside the cluster. Supply the cluster CA for its HTTPS endpoint; the bearer file is reread as it rotates. |
| `--k8s-namespace-policy-acknowledged` | false | Required operator acknowledgement that the dedicated namespace enforces step isolation. Verify CNI behavior separately. |
| `--allow-unrestricted-egress` | false | Explicitly permits networked steps; the launcher does not enforce manifest destination allowlists. |

### GKE

The launcher works the same on GKE, with these differences:

- A volume of a zonal Persistent Disk class (`WaitForFirstConsumer`) is created in the zone of its first Pod, the first transfer Pod, which asks for no GPU. A GPU step must then run in that zone. Keep the GPU node pool in one zone with the other nodes the runner's Pods can use, or give `--k8s-storage-class` a class whose `allowedTopologies` names the GPU zone.
- Enable network policy enforcement (Dataplane V2) before relying on `networkpolicy.yaml`, and set the cluster's Pod and Service ranges in the `cannery-network` ConfigMap (above).
- `cannery-steps-egress` blocks the metadata server (`169.254.169.254`) for steps with network; check it once from a `cannery.network: egress` Pod (`wget -T 5 http://169.254.169.254/` must fail). Bind no role and no cloud identity (Workload Identity) to the namespace's `default` ServiceAccount either way.
- `--k8s-gpu-runtime-class` stays unset (above), and the per-job volume has a minimum size (above).

## The stock policy

Every science revision says who verifies a run (`verify: {performer, verifier?}`), and attempts wait in `verifying` until a verification report is published. With performer `runner`, the stock policy is the runner's `verify` kind under a stock configuration ([job kinds](#job-kinds-and-the-configuration-file)): the kind claims the verify jobs registered to its service account through the HTTP API only (it never opens the database), runs the producer and the scorer through its launcher, applies the gates of its configuration file in-process and completes each job with the report: the gate results, the comparisons, a reason and the scorer's measurements. Create a verifier service account in the project whose name is the registered `verify.verifier.id`, give the configuration file the registered `verify.verifier.revision`, and pass its token as described in [Runner](#runner).

The `evaluator` command of the same image applies a stock configuration alone, offline: it never claims a job and needs no token, launcher, data root or Docker socket. A verifier that runs its own steps uses it to compute the gates, comparisons, verdict and reason of its report from the scorer's measurements (`cannery evaluator --help` lists its arguments). It refuses an invalid configuration, or a policy step file (exit code 2): a policy step runs through a launcher, so it runs as a `verify` kind of `cannery runner`.

The configuration format is described in [the contracts](contracts.md#verification-policy-and-the-stock-policy); `examples/fixture/policy.json` is a complete example. A job registered to another verifier or revision that reaches a runner anyway is failed with `policy_mismatch`; a policy step that gives no valid verdict fails the job with the step's failure code (`invalid_step_output` for an invalid verdict), which follows the rerun rules of the verify job. Changing the gates or baseline values means a new configuration revision and a new science revision registering it. To switch, add a `verify` entry with the new file (same token) once the new science revision is registered, and remove the old one once no job of the old revision is pending: each entry only ever receives its own revision's jobs, so running both side by side never fails or reruns a job.

**Policy revisions and stalled verifications.** A verifier's claims carry the policy revision it applies: `POST /api/projects/<slug>/jobs/claims` with `{"phase": "verify", "revision": "<verify.verifier.revision>"}`; a verifier's claim without it gets a 422. An agent's or a researcher's claim names no revision (`{"phase": "verify"}`). A verify job nobody has claimed for `CANNERY_LEASES_STALLED_VERIFICATION_SECONDS` (an hour by default) shows on the project's attention summary as a stalled verification, naming the hypothesis, the attempt, the performer and, for a runner job, the verifier and revision it waits for. Attempts that were waiting for a test or an evaluation when the installation moved to verify jobs were failed into failure review with the code `superseded`; a researcher's `retry` queues a fresh verify job from the run.

**Output checks.** Step outputs are checked against the interface the science revision registers for them ([the contracts](contracts.md#step-manifests-and-the-container-contract)): files must be non-empty unless the interface allows it and start as their media type says, and the JSON of a `schema` interface is validated against it. The runner fails a job whose outputs do not match, with `invalid_step_output`; run runners of the same release as the server. An agent verifier may name the `interface` of each output it uploads to have the API check it (`invalid_content` when it does not match); outputs uploaded without one are stored unchecked, with a null `content_validated`.

Outputs can fail these checks, for example JSON with `NaN` or `Infinity`, an empty output file, a JSON document over 64 MiB (declare `encoding: jsonl` or `validate: false` in the science revision), or JSON in UTF-16. Such a failure is blamed on the agent only when the API itself refused the producer's output as it was uploaded (the runner offers every failing producer output to the API for that): the attempt then goes straight to failure review with stage `agent`, without the automatic rerun, and a researcher's `retry` requeues the hypothesis. Anything the API holds no evidence for (a validator's rejection, a file over the API's validation cap, a verifier's report alone) is a verifier-side failure: the automatic rerun, then failure review with the reason preserved, where a researcher can retry. Uploads record a refusal and any bytes a failed upload left behind, which the background sweep deletes.
