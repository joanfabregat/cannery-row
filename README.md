<p align="center">
  <img src="web/public/logo.png" alt="Cannery Row logo: a smiling sardine standing on a tin" width="160">
</p>

<h1 align="center">Cannery Row</h1>

<p align="center">
  <a href="https://github.com/joanfabregat/cannery-row/actions/workflows/rust.yml"><img src="https://github.com/joanfabregat/cannery-row/actions/workflows/rust.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="https://github.com/joanfabregat/cannery-row/pkgs/container/cannery-row"><img src="https://img.shields.io/badge/image-ghcr.io%2Fjoanfabregat%2Fcannery--row-022b3b?logo=docker&logoColor=white" alt="Container image"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0%20%2F%20Apache--2.0-022b3b" alt="License: AGPL-3.0-only, contracts Apache-2.0"></a>
</p>

Cannery Row is a research workbench for experiment-driven work, such as AI and NLP research. Researchers and agents plan research tracks; a researcher approves each plan, and its units become the hypotheses that run. An outside agent, or a registered workflow run by the Cannery Row runner, performs the experiment and submits its artifacts and a run document: its claims and provenance as front matter, its run notes as the body. Independent testers re-run and grade the submission, an evaluator applies the project's policy and gives a verdict, and a researcher records the final decision with a reason. Cannery Row keeps the whole history (hypotheses, attempts, evidence, verdicts, decisions, comments) and serves it through its own web app (dashboard, review, comments and search), a REST API and an MCP server. One installation hosts several projects; the core knows nothing about any one project's metrics, datasets or code.

Cannery Row does not invent hypotheses or run an autonomous research planner, and the API process never executes project or candidate code: testers and evaluators pull their work and run it in containers.

## Status

Cannery Row is pre-1.0 and under active development. The API, the MCP server, the runner, the stock evaluator, the web app and the managed database work and are tested, but the contracts and settings can still change between releases.

## Quick start

Cannery Row is one binary, `cannery`, written in Rust. It embeds the web app, the database migrations and, optionally, its own PostgreSQL. Building it needs Rust 1.94.1 or later and Node 22 for the web app.

```sh
# The web app is embedded at compile time, so build it first.
(cd web && npm ci && npm run build)
cargo build --locked --release -p cannery --bin cannery
cp settings.example.toml settings.toml   # then edit it
```

With your own PostgreSQL server (tested with PostgreSQL 17; the `pg_trgm` extension must be available), set `[database] url` in `settings.toml`, then:

```sh
target/release/cannery --settings settings.toml migrate
target/release/cannery --settings settings.toml serve   # http://localhost:8000
```

Without a PostgreSQL server, set `[database] provider = "managed"`: `cannery` then runs a private PostgreSQL as its child process, under `database.data_dir`, and `serve` applies the migrations itself. Build with the bundled PostgreSQL (`--features bundled-postgres` and a bundle from `dev/build-postgres-bundle.sh`, see [operations](docs/rust/operations.md#bundled-postgresql)), or point `database.postgres_bin_dir` at an installed PostgreSQL 17:

```sh
target/release/cannery --settings settings.toml serve
```

Sign-in to the web app uses any standard OIDC provider (`[auth]` in `settings.toml`); `bootstrap_admin_emails` names the first administrators. For plain HTTP on `localhost`, set `[auth] cookie_secure = false`. The container image `ghcr.io/joanfabregat/cannery-row` runs the same binary; see [deployment](docs/deploy.md).

## Documentation

**[The integration guide](docs/guide.md)** is the first document to read, for an agent or an engineer: what to build and configure, in which order, to run a research project through Cannery Row end to end, with a pointer to the authoritative contract for each step.

| Document | Holds |
| --- | --- |
| [docs/guide.md](docs/guide.md) | Integrating a research project, from an empty installation to the first decided hypothesis. |
| [docs/agents.md](docs/agents.md) | Working as an agent: planning a track, and reading an attempt's context. |
| [docs/spec.md](docs/spec.md) | The system: roles, lifecycle, failure classes, trust rules. |
| [docs/contracts.md](docs/contracts.md) | Every document: tracks, track plans, hypotheses, uploads, evidence, jobs, step manifests, workflow tracks, the stock evaluator and policy steps. The JSON Schemas are in [`contracts/schemas/`](contracts/schemas). |
| [docs/deploy.md](docs/deploy.md) | The image, settings, object storage, the managed database, the runner, its launchers and the stock evaluator. |
| [docs/import.md](docs/import.md) | Importing a research history kept elsewhere. |
| [deploy/runner-k8s/](deploy/runner-k8s/README.md) | The runner's Kubernetes manifests. |
| [docs/rust/architecture.md](docs/rust/architecture.md) | The Cargo workspace, its crates and how a request moves through them. |
| [docs/rust/operations.md](docs/rust/operations.md) | Building, releases, the bundled PostgreSQL, the image and CI. |
| [docs/rust/limits.md](docs/rust/limits.md) | Request budgets, accepted encodings, schema and OIDC support, and runner policies. |

`examples/fixture/` is a complete small project that the tests use, and `examples/import/` a complete historical import bundle.

## Development

The workspace layout and the checks are described in [architecture](docs/rust/architecture.md) and [operations](docs/rust/operations.md); [CONTRIBUTING.md](CONTRIBUTING.md) has the short version. The web app in `web/` is React, TypeScript (strict) and Vite, with Tailwind CSS, shadcn/ui components on Radix primitives, TanStack Query, React Router, ESLint, Prettier and Vitest. Its API client is typed from the OpenAPI document (`web/openapi.json`) with `openapi-typescript` and `openapi-fetch`.

The scripts in `dev/` run every build and dependency command in containers, through `run-podman`, a rootless Podman wrapper; set `RUN` to use another runner with the same options. They read private values (the development domain, the OIDC client, the first admin's email) from the git-ignored `dev/local.env`; copy `dev/local.env.example` to start it.

| Script | What it does |
| --- | --- |
| `dev/local.sh` | `build`: builds the web app (if missing), the PostgreSQL bundle (if missing) and a release `cannery` with the bundled PostgreSQL. `serve`: runs it attached with a managed database and local storage, at `https://cannery.<DEV_DOMAIN>`, with sign-in through the OIDC client in `dev/local.env`. No argument does both. |
| `dev/web-fetch.sh` | Installs the web app's npm dependencies from the lockfile with install scripts disabled, then audits them (network, writable npm cache, nothing executed). |
| `dev/web-check.sh` | Web app install scripts (`npm rebuild`), lint, type check, tests, generated-types check and production build, offline. Run it after every `dev/web-fetch.sh`. |
| `dev/web.sh` | Vite dev server with hot reload at `https://cannery.<DEV_DOMAIN>`, proxying `/api`, `/auth` and `/mcp` to an API at `CANNERY_API_URL`, so the app, the OIDC callback and the session cookie share one origin. Needs `dev/web-fetch.sh`, then `dev/web-check.sh` once. |
| `dev/seed-demo.sh` | Loads three fictional demo projects (`dev/demo-data`: sentiment classification, a recommender, a demand forecast with an imported history) into the running `dev/local.sh serve` instance, so the web app has every screen populated: tracks in each state, plans and their reviews, running, submitted, tested and evaluated attempts, verdicts, decisions, failure reviews, comments, metrics and search content. It creates demo users and short-lived sessions in the managed database, imports the bundle with `cannery import`, and drives the rest through the REST API. `--help` has the details. |
| `dev/openapi.sh` | Regenerates `web/openapi.json` from the server's handlers and DTOs, and the typed client in `web/src/api/schema.d.ts`. Run it after changing an API model or route. |
| `dev/build-postgres-bundle.sh` | Builds the PostgreSQL bundle for the `bundled-postgres` feature: fetches and checks the pinned source and build tools, then runs `dev/postgres-bundle-build.sh` offline in a pinned build image. |
| `dev/postgres-bundle-build.sh` | The inner bundle build, run in the build container on Linux (and by CI), or directly on macOS. |
| `dev/common.sh` | Shared settings, sourced by the other scripts. |
| `dev/local.env.example` | Template for `dev/local.env`. |

## Licence

Cannery Row is licensed in two parts ([LICENSE](LICENSE)):

- The contracts under [`contracts/`](contracts) are licensed under the [Apache License 2.0](LICENSES/Apache-2.0.txt), so that anyone can implement and integrate with them.
- Everything else (the server, the runner, the command line and the web app) is licensed under the [GNU Affero General Public License, version 3 only](LICENSES/AGPL-3.0.txt).

Commercial licences for the AGPL parts are available from Joan Fabrégat. The project does not accept outside contributions yet; see [CONTRIBUTING.md](CONTRIBUTING.md).
