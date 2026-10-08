# Integrating a research project

This is the guide to read first. It tells an agent or an engineer what to build and configure, in which order, to run a research project's experiments through Cannery Row (CR), from an empty installation to the first decided hypothesis. It is the map and the procedure; the authoritative contracts stay in the references it links to:

| Reference | Holds |
| --- | --- |
| [spec.md](spec.md) | The system: roles, lifecycle, failure classes, trust rules. |
| [contracts.md](contracts.md) | Every document: tracks, hypotheses, uploads, evidence, jobs, step manifests, workflow tracks, the stock evaluator and policy steps. |
| [deploy.md](deploy.md) | The image, settings, object storage, the runner, its launchers and the stock evaluator. |
| [import.md](import.md) | Importing a research history kept elsewhere. |
| [deploy/runner-k8s/README.md](../deploy/runner-k8s/README.md) | The runner's Kubernetes manifests. |

The JSON Schemas under `contracts/schemas/` are the machine-checked contract; `examples/fixture/` is a complete small project that the test suite runs end to end. Every example below comes from it or follows its shape.

## The model in one screen

One installation hosts several **projects**, each fully separate: its own members, service accounts, configuration, tracks and history. Inside a project:

```text
project
├── science revision (immutable, versioned): metrics, datasets, interfaces, scorer, evaluator, limits…
├── producers and experiment steps (registered step manifests, each revision immutable)
└── track (agent or workflow mode)
    └── hypothesis  #12          draft → (human approval) → queued → active → … → decided
        └── attempt  #12.1       claimed under a lease, then three stages:
            1. experiment  → candidate artifacts + claimed result sheet   (agent, or runner's experiment kind)
            2. test        → evidence: tester-verified measurements        (runner's test kind)
            3. eval        → verdict: pass | fail | inconclusive            (runner's eval kind)
            then a human decision: promote | reject | inconclusive
```

- A **hypothesis** is written outside CR (by a researcher with an agent) and submitted as a `draft`. Nothing runs until a researcher approves it with a reason; it is then `queued`. CR never invents or recycles hypotheses.
- An **attempt** is one execution of a hypothesis. It is created by a **claim**, which returns a lease token; every write on the attempt needs the current token and generation. One attempt at a time per hypothesis.
- The three stages are kept apart on purpose. The **experiment** tests the hypothesis and produces the candidate and a **claimed result sheet** (its measurements are `agent_claim`, never trusted). The **test** re-runs and grades the frozen submission with trusted code and publishes `tester_verified` evidence. The **eval** applies the project's policy to that evidence and gives a verdict with a reason. A **human decision** comes last: `promote` needs a `pass` verdict; every decision needs a reason.
- A track's **mode** says only who runs the experiment stage: an outside agent (`agent`, the default) or a CR runner (`workflow`). From the submission on, both modes are identical.

Who holds which token, and what it may do:

| Identity | Token | May | May not |
| --- | --- | --- | --- |
| User, `viewer` | Personal token, or the web session | Read the project, reports, metrics, verdicts, decisions; search. | Download artifacts other than `report_asset`. |
| User, `member` | same | Viewer rights, plus comment and download artifacts. | Draft or decide. |
| User, `researcher` | same | Member rights, plus create and revise drafts, manage tracks (create, change mode, workflow or producer, pause, archive), claim in `agent` mode, and record every human decision. | Claim in `workflow` mode. |
| User, installation admin | same | Create projects, grant memberships, register science and dashboard revisions, producers and experiment steps, create service accounts and their tokens. Admin is not a project role: an admin also needs a membership to act as a researcher. | |
| Service account `agent` | Service token | Create and revise drafts, claim in `agent` mode, heartbeat, upload, post the manifest, submit, release; read the project. | Claim in `workflow` mode, comment, decide. |
| Service account `experimenter` | Service token, held by a runner only | Claim in `workflow` mode only, heartbeat, upload, submit, read the predecessor attempt's artifacts, release with a failure `code`, `step` and `logs` (trusted). | Draft, comment, decide, claim in `agent` mode. |
| Service account `tester` | Service token, named like the science revision's `tester.id` | Claim test jobs, read their inputs, upload outputs, complete or fail them. | Anything on attempts or decisions. |
| Service account `evaluator` | Service token, named like the science revision's `evaluator.id` | Claim evaluation jobs of its policy revision, complete or fail them; read the project. | Decide. |

A claimed job also returns a **job lease token**, which can only read that job's inputs and write under its output prefix. Personal and service tokens are created only from a signed-in browser session (`forbidden` otherwise), so an installation needs OIDC login configured ([deploy.md](deploy.md#configuration)) before anyone can mint a token. Every token carries the scopes `read` and/or `write`; writes need `write`. Service accounts act only in their own project.

### Which interface does what

| Interface | Use it for |
| --- | --- |
| Web app | Sign in; create projects; grant memberships; create service accounts and mint every token; create and change tracks; edit and review drafts; decide results and failures; read everything; comment; search. |
| REST (`/api/…`) | Everything, and the only way to register science and dashboard revisions (`POST /api/projects/{slug}/config/science`), producers (`…/producers`) and experiment steps (`…/experiment-steps`), and to create drafts besides MCP. Authenticate with `Authorization: Bearer <token>`. `GET /api/me` shows who a token is. |
| MCP (`/mcp`, Streamable HTTP, same bearer token) | An agent's work: `list_tracks`, `get_track`, `create_draft`, `revise_draft`, `search`, `claim_hypothesis`, `heartbeat_attempt`, `create_upload`, `post_manifest`, `submit_attempt`, `release_attempt`, `metric_catalog`, `query_metrics`, `query_comparisons`, and the researcher's `review_draft`, `record_decision`, `create_track`, `update_track`, `transition_track`. File bytes never go through MCP: `create_upload` returns a URL the client sends them to. |
| CLI (`cannery`) | `migrate`, `serve`, `db` (dump, restore, upgrade), `runner`, `evaluator` (the stock evaluator alone), `import`, `openapi`. |

Every error answer has the shape `{"error": {"code", "message", "details"}}`; `details` holds JSON Pointers into the request. See [Troubleshooting](#troubleshooting).

## Setting up a project

The order matters: each step is checked against the ones before it.

1. **Install and sign in.** Deploy the image and run `migrate` ([deploy.md](deploy.md#running)), with `CANNERY_AUTH_BOOTSTRAP_ADMIN_EMAILS` naming the first admin. Sign in once in the web app: users exist only after their first login.
2. **Create the project** (web app, or `POST /api/projects` with `{"slug", "title", "description"}`) and grant memberships (`PUT /api/projects/{slug}/members/{user_id}` with `{"role": "researcher"}`; `GET /api/users` finds a user id). Give yourself `researcher` if you will create tracks or decide.
3. **Register the science revision** (below).
4. **Register producers** and, for `workflow` tracks, **experiment steps** (below).
5. **Create the tracks.**
6. **Create the service accounts and their tokens.**

An admin's personal token with the `write` scope registers revisions and steps through REST:

```sh
API=https://cannery.example.org/api/projects/demo
curl -sf -X POST "$API/config/science" -H "Authorization: Bearer $ADMIN" \
  -H 'Content-Type: application/json' -d @examples/fixture/science.json
curl -sf -X POST "$API/producers" -H "Authorization: Bearer $ADMIN" \
  -H 'Content-Type: application/json' -d @examples/fixture/producers/overlap-producer.json
curl -sf -X POST "$API/experiment-steps" -H "Authorization: Bearer $ADMIN" \
  -H 'Content-Type: application/json' -d @examples/fixture/experiments/fixture-experiment.json
curl -sf -X POST "$API/tracks" -H "Authorization: Bearer $RESEARCHER" \
  -H 'Content-Type: application/json' -d @examples/fixture/workflow-track.json
```

### The science revision

The science revision is the project's rules, as one immutable JSON document (`science_revision.schema.json`). Every attempt pins the revision current at its claim, so a change is a new revision and never alters running or past work. `examples/fixture/science.json` is a complete one. What an ML project puts in it:

| Field | What to put there |
| --- | --- |
| `schema_version` | `"0.2"`. |
| `tester` | `{"id", "revision"?}`: the tester service account's name. The runner's `test` kind claims only jobs registered to its account's name. |
| `hypothesis_fields` | A JSON Schema for a hypothesis's `project_fields`. In a `workflow` track these are the experiment's **parameters** (learning rate, seed, model size); drafts are validated against it. |
| `metrics` | The metric registry: each `{key, unit, direction, aggregation, dimensions, splits, required_slices}`. Only registered metrics, splits and dimension values can be reported, charted or compared. |
| `datasets` | Each `{id, revision, held_out_labels, description?}`. Mark evaluation labels `held_out_labels: true` (see [the held-out labels rule](#the-held-out-labels-rule)). CR stores no dataset bytes: the runner reads them from its data root, `datasets/<id>/<revision>/`. |
| `baselines` | Controls, each `{id, revision, description?}`: immutable references a step can take as input (`from: baseline`, from `baselines/<id>/<revision>/`) and a hypothesis can name as its `control`. Their values live in the evaluator's configuration, not here. |
| `interfaces` | The formats steps exchange, `{name, version, schema | format, media_type?, encoding?, max_bytes?, allow_empty?, magic?, validate?, validator?}` ([contracts](contracts.md#step-manifests-and-the-container-contract)). Every step output naming an interface is checked against it. |
| `validators` | Optional `role: validator` step manifests that an interface names in `validator`. |
| `scorer` | The project's one scorer step manifest (`role: scorer`), shared by every track so metrics are computed identically. |
| `default_producer` | `{name, revision}` of the producer a track without its own binding uses. Register it right after the revision. |
| `code_repositories` | `{"candidate": [...], "trusted": [...]}`: the GitHub repositories (`owner/name`) steps may run code from, per trust class. A class not listed runs no repository code. |
| `evaluator` | `{id, revision}`: the evaluator service account's name and the policy revision its verdicts must report. Required. |
| `required_artifact_roles` | `{"attempt": [...], "tester": [...]}`: roles a submission's manifest and a tester's output manifest must include. |
| `limits` | `resource_ceilings` (per step; a step may only ask for a resource that has a ceiling, so list `nvidia.com/gpu` for GPU steps), `max_deadline_seconds` (every step's and setup's deadline must fit it; also an evaluation job's deadline, one hour when unset), `report_max_bytes` (at most 1 MiB), `max_output_bytes`. |
| `max_auto_retries` | Automatic reruns of a failed stage (and requeues of a failed runner-driven experiment) before a human failure review. Default 1. |
| `retention`, `result_extensions` | Optional. |

The registration checks the scorer and validators against the rest of the revision (interfaces, datasets, ceilings, repositories). The dashboard revision (`POST …/config/dashboard`, [contracts](contracts.md#project-configuration-and-dashboard-contract)) is optional and separate: without one the web app derives views from the metric registry.

### The evaluator and its policy

Every science revision names an evaluator; CR applies no policy itself. Decide which one before registering the revision ([Eval stage](#eval-stage)):

- the **stock evaluator**: a configuration file of declarative gates, such as `examples/fixture/evaluator.json`, whose `evaluator.revision` equals the science revision's `evaluator.revision`. It holds the baseline values the gates compare against;
- a **policy step**: a trusted step that computes the verdict, for paired bootstraps, significance tests or anything the gates cannot express.

Either way the policy file lives with the runner, not in CR: changing the policy means a new policy revision and a new science revision registering it.

### Tracks

A track is a line of research (one architecture against another). It is created by a researcher (web app, REST, or MCP `create_track`) from `examples/fixture/tracks.json`-style documents:

```json
{"slug": "lexical", "title": "Lexical overlap", "description": "Ranks documents by shared words.",
 "producer": {"name": "overlap-producer", "revision": 1}}
```

- `producer` binds the producer that tests this track's candidates; omitted, `default_producer` applies. Its output interface must equal the scorer's `from: step` input interface.
- `mode` is `agent` (default) or `workflow`; a `workflow` track also names `workflow: {"steps": [{"name", "revision"}, …]}` (1 to 16 registered experiment steps, run in order). See `examples/fixture/workflow-track.json` and [the rules](contracts.md#the-workflow).
- Changing `producer`, `mode` or `workflow` is `PATCH /api/projects/{slug}/tracks/{track}` with `expected_revision`, the fields and a non-empty `reason`; attempts already claimed keep what they pinned.
- States are `active`, `paused` (nothing is claimed) and `archived`, changed with `POST …/tracks/{track}/transitions` (`to_state`, `expected_revision`, `reason`).

### Service accounts and tokens

Create one service account per identity in the project's admin settings (or `POST /api/projects/{slug}/service-accounts` with `{"kind", "name", "description"}`), then mint its token in the web app (`name`, `expires_in_days`, `scopes`; the secret is shown once):

| Kind | Name | Token goes to |
| --- | --- | --- |
| `tester` | The science revision's `tester.id` (`cannery-runner` in the fixture). | The runner's `test` kind. |
| `evaluator` | The science revision's `evaluator.id` (`stock-evaluator` in the fixture). | The runner's `eval` kind (or `cannery evaluator`). |
| `experimenter` | Any. | The runner's `experiment` kind, only for `workflow` tracks. Never to an agent: its failure reports are trusted. |
| `agent` | Any, one per agent if you want them told apart. | An outside agent working `agent` tracks. A researcher working through an agent may use a personal token instead; the agent then shows in `via`. |

Give each token `read` and `write`. A runner reads each token from its own file, with no permission for group or others (for example `600` or `400`).

## Steps

A step is one container run described by a **step manifest**, like a GitHub Actions job: a stock image pinned by digest, a script from a repository at a pinned commit, and a cached dependency install. Field names follow Argo Workflows templates. The full contract is [Step manifests and the container contract](contracts.md#step-manifests-and-the-container-contract) and [Steps as scripts](contracts.md#steps-as-scripts-code-setup-and-the-dependency-cache).

| Field (under `spec`) | Meaning |
| --- | --- |
| `role` | `producer`, `scorer`, `validator`, `experiment` or `evaluator`. |
| `container` | `image` (pinned by digest, `…@sha256:<64 hex>`; a tag is refused), `command`, `args`, `env` (no secrets, no `NVIDIA_*`), `resources.limits` (within the ceilings). |
| `activeDeadlineSeconds` | The step's deadline, counted from its start (not the image pull). |
| `network` | `none`, or `{"egress": ["host:port", …]}`. The allowlist is not enforced by the Docker or Kubernetes launcher yet: declared egress reaches any outside destination. |
| `sandbox` | Free text: the sandbox and capability policy the step declares. |
| `inputs.artifacts` | Each `{name, from, path, id?, interface?}`, `from` one of `attempt`, `dataset`, `baseline`, `step`. A dataset or baseline input reads the registered id in `id`, or the one equal to its `name` without it. `name` is snake case and stays the input's directory and `job.json` key, so give `id` for an id with a hyphen: `{"name": "qrels", "from": "dataset", "id": "nanobeir-qrels", "path": "/cr/inputs/qrels"}`. Only dataset and baseline inputs take `id`. |
| `outputs.artifacts` | Each `{name, path, interface?}`. |
| `code` | `{repo, commit, path?}`: run a script from `repo` (`owner/name`) at a full 40-character `commit`, with `path` mounted at `/cr/code`. |
| `setup` | `{run, cache: {key_files, paths?}, network?, activeDeadlineSeconds?}`: install dependencies once per cache key. Requires `code`. |

### Trust classes

| Class | Roles | Code from | Sees held-out labels |
| --- | --- | --- | --- |
| `candidate` | `producer`, `experiment` | `code_repositories.candidate` | Never. |
| `trusted` | `scorer`, `validator`, `evaluator` | `code_repositories.trusted` | The scorer and a policy step may. |

A candidate step's setup cache never serves a trusted step, even with the same image and lockfile. The runner can narrow the repositories further with `--github-allowed-repos` ([Running the runner](#running-the-runner)).

### The `/cr` layout

| Path | Contents |
| --- | --- |
| `/cr/job.json` (read-only) | The job or attempt, the step, its manifest and, for an experiment step, the hypothesis's `parameters`. Never a token. |
| `/cr/inputs/<name>/` (read-only) | Each declared input, SHA-256-verified before the step starts. |
| `/cr/outputs/<name>/` | Each declared output, checked against its interface after the step exits, then uploaded. |
| `/cr/code` (read-only) | With `code`: the repository tree at the commit (only `code.path`); the working directory. |
| `/cr/cache` (read-only) | With `setup`: what setup installed. |
| `/tmp` | Writable scratch. With the Docker and Kubernetes launchers `HOME` and `TMPDIR` are `/tmp`; with the local launcher `HOME` is the per-step root (`$CR_ROOT`) and `TMPDIR` is `$CR_ROOT/tmp`. |

Exit 0 is success; anything else, a deadline, an out-of-memory kill or a missing declared output is a failure. The step's stdout and stderr become its `step_log` artifact. A step has no credentials and never calls the API. With the local launcher, `/cr` is a per-step directory given in `CR_ROOT`, so write steps as `root = os.environ.get("CR_ROOT", "/cr")` to run on every launcher.

An **interface** check applies to every output that names one: presence, non-empty unless `allow_empty`, `max_bytes`, `magic`, then JSON or JSON Lines validation against `schema`, then the interface's `validator` step if any. A validator exits `0` to accept, `1` to reject, anything else when it could not check.

### The held-out labels rule

A dataset marked `held_out_labels: true` reaches only trusted judges: the scorer and an evaluator policy step. A producer or experiment step that declares one as input is refused at registration (`validation_failed`), when a track binds or names it, at claim, and again by the runner before the step runs (`held_out_labels_to_producer`, `held_out_labels_to_experiment`). An experiment step's `job.json` never lists one. The local launcher does not enforce this (its steps can read the whole data root), which is why it is for trusted fixture code only.

### The setup cache

On the first run of a key, setup runs in the step's image with only the `key_files` in `/cr/code`, an empty writable `/cr/cache`, its own network, and nothing else: no `job.json`, inputs, datasets or credentials. The key is the SHA-256 of the image digest, `setup.run`, the repository and `code.path`, the step's `env`, the setup's network, the trust class, and each key file's path and SHA-256. The commit is not in the key: a new commit with the same lockfile reuses the cache. Consequences:

- install from the lockfile only, never the project itself (`pip install .`, `npm install` of the package);
- list a setup script in `key_files`, or setup cannot see it;
- no Python virtual environment in the cache (its interpreter link points outside it): install with `pip --target`;
- variables in `env` that change per run (a seed) also change the key: keep them out of a step with a setup.

The runner caps the cache root (`cache_max_bytes`, `20Gi` by default), evicting least recently used entries.

### Worked example: a Python experiment step

An experiment that fine-tunes a model with the hypothesis's parameters, from `example-org/ranker` (listed in `code_repositories.candidate`). The repository's `experiments/` holds `train.py` and a hash-pinned `requirements.txt` (`pip-compile --generate-hashes`).

```yaml
apiVersion: cannery-row/v1
kind: Step
metadata:
  name: finetune
spec:
  role: experiment
  code:
    repo: example-org/ranker
    commit: 4f2a9c1e8b7d6a5f4e3d2c1b0a9f8e7d6c5b4a39   # git rev-parse HEAD; never a branch
    path: experiments
  setup:
    run: >-
      pip install --require-hashes --no-deps --only-binary :all: --no-cache-dir
      --target /cr/cache/site -r requirements.txt
    cache: {key_files: [requirements.txt], paths: [site]}
    network: {egress: ["pypi.org:443", "files.pythonhosted.org:443"]}
    activeDeadlineSeconds: 900
  container:
    image: docker.io/library/python@sha256:7c61056e61ac89e852de05f3dc6fa51a6dd2181797bceed46aa725dd7cb2cd3b
    command: [python, train.py]
    env: [{name: PYTHONPATH, value: /cr/cache/site}]
    resources: {limits: {cpu: "8", memory: 32Gi, nvidia.com/gpu: "1"}}
  activeDeadlineSeconds: 14400
  network: none
  sandbox: "Stock Python image; code and dependencies read-only; no network."
  inputs:
    artifacts:
      - {name: checkpoint, from: attempt, path: /cr/inputs/checkpoint}   # the predecessor's, if any
      - {name: train, from: dataset, path: /cr/inputs/train}
  outputs:
    artifacts:
      - {name: checkpoint, interface: checkpoint/v1, path: /cr/outputs/checkpoint}
      - {name: claimed_sheet, interface: cr-evidence/v0.2, path: /cr/outputs/claimed_sheet}
```

```python
# experiments/train.py
import json
import os
from pathlib import Path

from ranker import train  # the project's own code, next to train.py in /cr/code

root = Path(os.environ.get("CR_ROOT", "/cr"))
job = json.loads((root / "job.json").read_text())
params = job["parameters"]  # the hypothesis's project_fields
resume = root / "inputs" / "checkpoint"  # empty on a first attempt
model = train(root / "inputs" / "train", lr=params["learning_rate"], resume=resume)
model.save(root / "outputs" / "checkpoint" / "model.safetensors")
(root / "outputs" / "claimed_sheet" / "claimed_sheet.json").write_text(
    json.dumps(
        {
            "report": {
                "what_was_tried": f"Fine-tune with lr={params['learning_rate']}.",
                "configuration": "Base checkpoint, train split train-r3, 3 epochs.",
                "observations": "Loss plateaued after epoch 2.",
                "findings": "Dev loss improved; the test stage will measure retrieval quality.",
                "limitations": "One seed.",
                "next_question": "Does a lower learning rate keep the gain?",
                "elapsed_seconds": 5400,
                "body_markdown": "# Fine-tune\n\nFull report…",
            },
            "artifact_roles": ["checkpoint"],
        }
    )
)
```

This needs, in the science revision: a `checkpoint/v1` interface (for example `{"name": "checkpoint", "version": 1, "format": "safetensors", "media_type": "application/octet-stream"}`), a `train` dataset not marked as held-out labels, `learning_rate` in `hypothesis_fields`, a `nvidia.com/gpu` ceiling, and `max_deadline_seconds` of at least 14400. The track's producer then reads `checkpoint` (`from: attempt`).

### Worked example: a Node validator step

A trusted check on the producer's `ranked-run/v1` output, from `example-org/scoring` (listed in `code_repositories.trusted`), named by the interface's `validator`. It is registered inside the science revision's `validators`, not through an endpoint.

```json
{
  "apiVersion": "cannery-row/v1",
  "kind": "Step",
  "metadata": {"name": "run-validator"},
  "spec": {
    "role": "validator",
    "code": {"repo": "example-org/scoring", "commit": "9b8a7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b", "path": "validators"},
    "setup": {
      "run": "mkdir -p /cr/cache/npm && cp package.json package-lock.json /cr/cache/npm/ && npm ci --ignore-scripts --no-audit --no-fund --cache /cr/cache/.npm --prefix /cr/cache/npm && rm -rf /cr/cache/.npm",
      "cache": {"key_files": ["package.json", "package-lock.json"], "paths": ["npm/node_modules"]},
      "network": {"egress": ["registry.npmjs.org:443"]}
    },
    "container": {
      "image": "docker.io/library/node@sha256:43ac6c60b8f89723f746e8a92ce91abd5017e627ce1ddfe4238355d3a30b772c",
      "command": ["node", "check-run.js"],
      "env": [{"name": "NODE_PATH", "value": "/cr/cache/npm/node_modules"}],
      "resources": {"limits": {"cpu": "1", "memory": "512Mi"}}
    },
    "activeDeadlineSeconds": 120,
    "network": "none",
    "sandbox": "Reads only the output it checks, read-only; no network.",
    "inputs": {"artifacts": [{"name": "run", "from": "step", "interface": "ranked-run/v1", "path": "/cr/inputs/run"}]},
    "outputs": {"artifacts": []}
  }
}
```

```js
// validators/check-run.js (CommonJS, so NODE_PATH finds the cached dependencies)
const fs = require("fs");
const path = require("path");
const dir = path.join(process.env.CR_ROOT || "/cr", "inputs", "run");
try {
  for (const name of fs.readdirSync(dir)) {
    const run = JSON.parse(fs.readFileSync(path.join(dir, name), "utf8"));
    for (const [query, docs] of Object.entries(run.queries)) {
      if (new Set(docs).size !== docs.length) {
        console.error(`${name}: query ${query} ranks a document twice`);
        process.exit(1); // the output is rejected
      }
    }
  }
} catch (err) {
  console.error(`cannot check: ${err.message}`);
  process.exit(2); // the validator failed; says nothing about the output
}
```

The image digest is that of `node:22-slim` when this guide was written; pin the digest you have checked. ES modules ignore `NODE_PATH`: import dependencies by their path under `/cr/cache/npm/node_modules`, or commit a bundle.

## Experiment stage

### Agent mode

An outside agent (a Codex or Claude Code session) runs the experiment itself, through REST or MCP, with an `agent` service token or a researcher's personal token.

1. **Draft.** `POST /api/projects/{slug}/hypotheses` (MCP `create_draft`) with a hypothesis document (`examples/fixture/hypothesis.json`), optionally with an `Idempotency-Key` header. Search first (`GET /api/search`, MCP `search`) for prior related work. A researcher approves it (`POST …/hypotheses/{number}/draft-review` with `{"draft_revision", "action": "approve", "reason"}`, or the web app).
2. **Claim.** `POST /api/projects/{slug}/claims` with `{}` or `{"hypothesis": 12}` or `{"track": "lexical"}` (MCP `claim_hypothesis`). The answer holds the `attempt`, `lease_token` and `lease_generation` (send them as `X-Lease-Token` and `X-Lease-Generation` on every attempt call) and `heartbeat_seconds`.
3. **Heartbeat** `POST …/hypotheses/{number}/attempts/{sequence}/heartbeat` at least every `heartbeat_seconds` (a third of the lease TTL, `leases.ttl_seconds`, 900 seconds by default).
4. **Upload** each file: `POST …/attempts/{sequence}/uploads` with `{role, name, size_bytes, sha256, media_type}`, then send the bytes as the grant says (a `PUT upload_url`, or the `direct` presigned requests and `POST finish_url`; [the protocol](contracts.md#uploads-and-downloads)). The role is what the track's producer reads (`from: attempt`), plus every role in `required_artifact_roles.attempt`.
5. **Post the manifest** of the verified uploads (`POST …/manifest`, MCP `post_manifest`), which answers `{ref, sha256}`.
6. **Submit** the claimed result sheet (`POST …/submission` with an `Idempotency-Key`, MCP `submit_attempt`): an evidence envelope with `stage: "agent"`, the `report` and the manifest reference ([the sheet](contracts.md#evidence-envelope)). The attempt is frozen and the test job is queued.

A sheet the API rejects fails the attempt at once (`invalid_submission`) and opens a failure review: validate it against `evidence_envelope.schema.json` before submitting (`GET /api/schemas/run` serves it as one self-contained schema, no token needed). To give up, `POST …/release` with a `reason` (MCP `release_attempt`): the attempt fails with `released` and a failure review case opens. An agent has no attempt deadline, only the lease.

### Workflow mode

A runner's `experiment` kind runs the experiment with an experimenter token. Setting it up takes four things ([workflow tracks](contracts.md#workflow-tracks)): `hypothesis_fields` in the science revision describing the parameters, registered experiment steps, a `workflow` track naming them, and a runner with an `experiment` kind.

- The workflow's last step, and only it, outputs `claimed_sheet` (`cr-evidence/v0.2`): one JSON object with `report` and optionally `measurements` (`agent_claim`), `observations`, `artifact_roles`, `extensions`, `provenance`. The runner fills in the rest. Every other output becomes attempt artifacts whose role is the output's name (flat files only), and together they must cover `required_artifact_roles.attempt` and every `from: attempt` input of the track's producer.
- The hypothesis supplies the parameters: the `project_fields` of its approved revision arrive as `parameters` in the claim's `workflow` object and in each step's `/cr/job.json` ([job.json](contracts.md#jobjson-of-an-experiment-step)). Hypotheses are drafted with them (`examples/fixture/workflow-hypothesis.json`, `"project_fields": {"top_k": 2}`) and approved as in any track.
- A `from: attempt` input of an experiment step reads the **predecessor** attempt's verified artifacts of that role (an empty directory on a first attempt), so a step can resume from a failed attempt's checkpoint. The step decides whether a partial predecessor output is usable.
- The runner claims with `{"mode": "workflow"}`, heartbeats, runs the steps in order through its launcher, checks each output against its interface and validator, uploads every output and log, posts the manifest and submits. The attempt's deadline is pinned at claim: the sum of the steps' deadlines (setups and validators included) plus `leases.job_overhead_seconds` (300 by default).
- A claim skips a track whose workflow or producer no longer fits the current science revision. When only such tracks have queued hypotheses the claim answers `409 workflow_unavailable` naming them; the runner logs it and keeps polling until a researcher fixes the track or the science revision.

### Crashes, lease loss and requeue

| What happens | Agent mode | Workflow mode |
| --- | --- | --- |
| The worker stops heartbeating (crash, lost network) | The sweep fails the attempt with `lease_expired`, an agent-side failure: a failure review case opens. A researcher's `retry` queues the hypothesis again; the next claim creates a new attempt linked to the failed one. | The sweep fails it with `lease_expired`, a failure of the run: the hypothesis is queued again automatically, with no review case, while `max_auto_retries` allows; then a failure review case. |
| The attempt passes its deadline | No deadline. | The API refuses the lease (`stale_lease`) and the sweep fails it with `deadline_exceeded`, then as above. |
| A late call after the lease was lost | `409 stale_lease`: stop working on the attempt. | The runner stops the step and reports nothing. |
| The worker gives up | `release`: fails with `released`, review case. | The runner releases with a `code` (`step_failed`, `setup_failed`, `invalid_step_output`… [the list](contracts.md#failures-release-codes-and-automatic-retries)), the failing `step` and its `logs`; requeued automatically as above. |
| An upload the API refused or could not verify, or a sheet it rejected | `upload_verification_failed` or `invalid_submission`: the candidate's failure, review case at once. | The same: these are blamed on the candidate in both modes. |
| The runner gets `SIGTERM` | | It cancels the run, removes its containers, reports nothing and exits 143; the lease expires and the sweep requeues the hypothesis. |

## Test stage

Submission queues a test job. The runner's `test` kind claims it with the tester token and runs a chain of steps:

1. the track's **producer** (candidate code, the only step of the test that executes the candidate): it reads the submission's artifacts (`from: attempt`) and datasets that are not held-out labels, and writes an intermediate output naming an interface, for example ranked results or predictions per example;
2. any **validator** that output's interface names;
3. the project's **scorer** (trusted): it reads the producer's output (`from: step`, the same interface), the held-out labels (`from: dataset`) and the frozen `claimed_sheet` (`from: attempt`), and writes `evidence`, the tester evidence envelope (`cr-evidence/v0.2`), plus outputs such as `per_query_results`.

Register producers with `POST /api/projects/{slug}/producers`; each registration of a name is its next immutable revision, so a new producer for a new track never mints a science revision. The scorer lives in the science revision so every track is scored the same way. A self-hosted tester can implement the job API instead of the runner ([test and evaluation jobs](contracts.md#test-and-evaluation-jobs)).

The **evidence envelope** ([contracts](contracts.md#evidence-envelope)) has `stage: "tester"`, `provenance` (`source_revision`, `tester_revision`, `dataset_revision`, `control_revision` when the hypothesis names a control, `science_revision`), `measurements` (each `authority: "tester_verified"`, finite values, registered metric, split and dimensions, a `missing_reason` instead of a value when it could not be measured), `discrepancies` with the claimed sheet, and `artifact_roles`. A required slice `{dimension: value}` is covered only by a measurement whose `dimensions` are exactly that pair. CR validates the envelope and the output manifest, then queues the evaluation job.

The job's deadline is the sum of its steps' deadlines (setups and validators included) plus `leases.job_overhead_seconds`. A failure of the test (a step exits non-zero, a deadline, a validator rejection, an invalid scorer output) is an infrastructure failure of the stage: it reruns on the same frozen submission while `max_auto_retries` allows, then opens a failure review. The exception is a producer output the API itself refused against its interface (`invalid_step_output`): that is the candidate's failure, reviewed at once, without a rerun.

## Eval stage

The runner's `eval` kind claims evaluation jobs pinned to its policy's revision, with the evaluator token, and completes each with a verdict (`pass`, `fail` or `inconclusive`), each gate's result (`pass`, `fail`, `unknown`), optional comparisons, and a reason. CR checks only that the verdict comes from the registered evaluator under the registered revision and that a `pass` reports every gate as passed. Every completed evaluation, whatever its verdict, leaves the attempt awaiting a human decision.

An evaluation job pins the hypothesis's `parameters` when it is created (the `project_fields` of the revision the attempt pinned, `{}` without any), so a later revision never changes what a run or rerun sees: a policy step reads them from `/cr/job.json` ([contracts](contracts.md#policy-steps)) and the stock evaluator ignores them.

| | Stock gates | Policy step |
| --- | --- | --- |
| `policy` file | A stock configuration: `{schema_version, evaluator {id, revision}, gates, baselines, default_control?}` (`examples/fixture/evaluator.json`, [contracts](contracts.md#evaluation-policy-and-the-stock-evaluator)). | A policy step document: exactly `{schema_version, evaluator {id, revision}, step}` (`examples/fixture/policy-step.json`, [contracts](contracts.md#policy-steps)). |
| Runs | In the runner process; no launcher, no data root. | Through the launcher, as a trusted step: `role: evaluator`, `network: none` for the step (its setup may have network), exactly one output named `verdict`, no `from: step` input, `code.repo` in `code_repositories.trusted`. It may read the tester's evidence (`evidence`), its output manifest (`manifest`), any tester output role, datasets (held-out labels included) and baselines. |
| Writes | The gate results it computes. | One JSON file in `/cr/outputs/verdict/`, at most 1 MiB, with exactly `gates`, `comparisons` (optional), `verdict` and `reason`. |
| Alone | `cannery evaluator --config evaluator.json`. | Only as an `eval` kind of `cannery runner`. |

Failures of the eval stage follow the rerun rules, then a failure review:

- `policy_mismatch`: the job is pinned to another evaluator id or revision than the policy file's. The claim hands out only the evaluator's own revision, so this signals a misconfiguration;
- `input_verification_failed`: evidence or the manifest does not match the digests the job pins;
- `evaluator_error`: anything else, a step that exits non-zero, runs out of time or memory, writes no verdict or an invalid one, or does not fit the science revision or the job's deadline. The reason starts with the underlying code (`step_failed: …`, `deadline_exceeded: …`).

**The deadline budget.** An evaluation job's deadline is the science revision's `limits.max_deadline_seconds` (one hour when unset). A policy step's setup deadline (when it has a setup) plus its `activeDeadlineSeconds` plus the 30 seconds the runner keeps to upload and report must fit it, or the step never starts and the job fails with `evaluator_error`.

To switch policy revisions without stopping evaluation, register the new science revision, run a second `eval` entry with its own `name` and the new policy (the same token), and remove the old entry once no job of the old revision is pending.

## Running the runner

`cannery runner --config runner.toml` runs every kind a project needs in one process ([job kinds](deploy.md#job-kinds-and-the-configuration-file)). The flags alone run only the `test` kind; `--config` replaces every flag but `--once`. A complete file, with the Docker launcher:

```toml
api_url = "https://cannery.example.org"
project = "demo"
data_root = "/var/lib/cannery/data"     # datasets/<id>/<revision>/, baselines/<id>/<revision>/
work_root = "/var/lib/cannery/work"     # per-job directories; the cache defaults to <work_root>/cache
cache_max_bytes = "50Gi"

[launcher]
type = "docker"
runner_id = "runner-1"                  # unique per Docker daemon or namespace, stable across restarts
docker_gpu_mode = "cos"                 # on Container-Optimized OS; "nvidia" elsewhere (default)
docker_gpu_devices = [0]                # the GPUs this runner lends to steps

[github]
app_id = "123456"
app_key_file = "/run/secrets/github-app.pem"
app_installation_id = "7654321"
allowed_repos = ["example-org/ranker", "example-org/scoring"]

[[kinds]]
kind = "experiment"
token_file = "/run/secrets/experimenter.token"

[[kinds]]
kind = "test"
token_file = "/run/secrets/tester.token"
concurrency = 2                         # two test jobs at once; size the host for the sum

[[kinds]]
kind = "eval"
token_file = "/run/secrets/evaluator.token"
policy = "/etc/cannery/policy-step.json"   # or a stock configuration such as evaluator.json
poll_seconds = 30
```

- **One token file per kind.** Each is a regular file with no permission for group or others (for example `600` or `400`); the runner checks the mode, not the owner, so the file must belong to the user the runner runs as (UID 10001 in the image). It refuses a file that grants group or others any permission, and one token given to kinds of different types. Two `eval` entries may share one.
- `concurrency` (default 1) runs that many loops of the entry; resource ceilings apply per step, so several loops add up. `poll_seconds` (default 10) is the wait when nothing is queued or a claim failed.
- A kind that runs steps (`test`, `experiment`, an `eval` policy step) needs `[launcher]`; `test` and `experiment` need `data_root`, and so does a policy step that reads a dataset or baseline.
- An invalid file, an unreadable token or a missing launcher or data root refuses to start with exit code 2, naming the key (`kinds[1].token_file: …`).
- `--once` runs at most one job per entry, prints each outcome (`no job waiting`, `job <id> completed`) and exits 1 if any claim or job raised. `SIGTERM` cancels running jobs, removes their containers or Pods and exits 143; the cancelled jobs are not reported and are claimed again once their lease expires.

### Launchers

| Situation | Launcher | Where it is set up |
| --- | --- | --- |
| The `examples/fixture/` project, development, CI, trusted code you wrote. | `local` (`--unisolated-local`): steps run as local processes, as the runner's user, from a fresh copy of `step_root`; images are ignored; nothing is isolated, held-out labels and token files included. | [deploy.md](deploy.md#runner) |
| Untrusted candidate code on one dedicated machine, including GPUs on a single VM. | `docker`: one container per step, non-root, read-only root filesystem, no capabilities, `network: none` enforced. Whoever reaches the Docker socket is root on the host, so the VM is the isolation boundary. | [deploy.md](deploy.md#the-runner-on-a-container-optimized-os-vm) |
| GPU steps on a COS VM. | `docker` with `docker_gpu_mode = "cos"` (`--docker-gpu-mode cos`) and `--docker-gpu-devices 0,1`: each GPU is lent to one step at a time; a step waits for free GPUs before its deadline starts. | [deploy.md](deploy.md#gpus) |
| Untrusted code in a Kubernetes cluster, scaled or shared GPU pools. | `kubernetes`: one Pod per step on a per-job volume, Pod Security `restricted`, NetworkPolicy deny-all for `network: none`. Needs Kubernetes 1.30+, a StorageClass and enforced NetworkPolicy. | [deploy.md](deploy.md#the-runner-on-kubernetes), [deploy/runner-k8s/](../deploy/runner-k8s/README.md) |
| Only the stock evaluator. | None: `cannery evaluator`, or an `eval` kind with a stock configuration. | [deploy.md](deploy.md#the-stock-evaluator) |

The shipped Kubernetes manifests run the `test` kind with flags; to run all three kinds there, mount a configuration file and copy each token as its init container does ([with a configuration file](deploy.md#with-a-configuration-file)).

### GitHub code access

Steps with `code` need the runner to fetch the commit. Public repositories need no credential. For private ones, give the runner a GitHub App with **Contents: Read-only** (`[github] app_id`, `app_key_file`, `app_installation_id`; recommended, tokens refresh) or a read-only token file (`[github] token_file`). The credential stays in the runner's memory; no step sees it. The commit must be on a branch of `code.repo` itself, not only of a fork. `allowed_repos` (`--github-allowed-repos`) narrows what the science revision allows: a step outside it fails with `code_not_allowed`. Setup: [the runner's GitHub credential](deploy.md#the-runners-github-credential).

### Cache limits

The cache root (`cache_root`, `<work_root>/cache` by default) holds code trees by `repo@commit` and setup outputs by key, capped by `cache_max_bytes` (`20Gi` by default), least recently used first. Each runner needs its own cache root. With the Docker launcher and `docker_job_root_host`, keep the cache root under `work_root`. Details: [the code and dependency cache](deploy.md#the-code-and-dependency-cache).

## Importing history

If the project has results from before CR (notebooks, reports, run artifacts in a bucket), import them once, before or beside live work, so new hypotheses can be compared with them. `cannery import --bundle DIR --project SLUG [--science FILE] [--dry-run] [--allow-missing]` loads a human-reviewed bundle of YAML or JSON files in one transaction ([import.md](import.md); `examples/import/` is a complete bundle for the fixture science revision). Always start with `--dry-run`: an imported file can never be edited afterwards.

Imported records carry `origin: imported` and a `source_ref`; their measurements have the authorities `imported_artifact` (read from an artifact) or `imported_transcribed` (copied from a document), never `tester_verified`, so they never pass for work CR tested. A finished imported attempt with no decision of its own is `unreviewed`, a state only imports use. An imported draft or a result awaiting review continues in the live workflow. Imported artifacts are references (backend `external`, URI, size, SHA-256): CR never had the bytes. Metrics queries and comparisons return imported values only on request (`authority=imported`, `origin=imported`).

An attempt may carry its retrospective report, a Markdown file under the bundle's `reports/`: it is shown on the attempt as imported history ("Retrospective report", with its author and date), never as an agent's report or as evidence.

## Checklist

From an empty CR to the first decided hypothesis:

1. Deploy the image with OIDC and a bootstrap admin, run `migrate`, sign in. [deploy.md](deploy.md#configuration)
2. Create the project and grant `researcher` to the people who approve and decide (yourself included). [Setting up a project](#setting-up-a-project)
3. Write the scorer, its interfaces and validators; choose the evaluator (stock gates or policy step) and write its policy file. [Steps](#steps), [Eval stage](#eval-stage)
4. Register the science revision. [The science revision](#the-science-revision)
5. Register the producers (the default first) and, for workflow tracks, the experiment steps. [Test stage](#test-stage), [Workflow mode](#workflow-mode)
6. Create the tracks, each with its mode. [Tracks](#tracks)
7. Create the tester, evaluator and, for workflow tracks, experimenter service accounts, named as the science revision says, and an agent account for agent tracks; mint their tokens in the web app. [Service accounts and tokens](#service-accounts-and-tokens)
8. Put the datasets and baselines in the runner's data root at `datasets/<id>/<revision>/` and `baselines/<id>/<revision>/`. [deploy.md](deploy.md#runner)
9. Write `runner.toml`, choose the launcher, give it the GitHub credential, start `cannery runner --config runner.toml` and check it with `--once`. [Running the runner](#running-the-runner)
10. Optionally import the history. [Importing history](#importing-history)
11. Draft a hypothesis and approve it. [Agent mode](#agent-mode)
12. Run the experiment: an agent claims and submits, or the experiment kind does. [Experiment stage](#experiment-stage)
13. Watch the test and evaluation jobs (`GET …/attempts/{sequence}/jobs`, the web app's attempt page).
14. A researcher reviews the result case (`GET /api/projects/{slug}/review-cases`, then `POST …/review-cases/{case_id}/decisions` with `review_case_id`, `evidence_revision`, `action` and `reason`, or the web app) and records `promote`, `reject` or `inconclusive`.

## Troubleshooting

| Code | Where | Means | Do |
| --- | --- | --- | --- |
| `nothing_to_claim` (409) | Hypothesis claim | No queued hypothesis in an active track of the caller's mode. | Check a draft was approved, the track is `active`, and the identity matches the mode (agents claim `agent` tracks, experimenters `workflow` tracks). The runner just keeps polling. |
| `workflow_unavailable` (409) | Hypothesis claim, `workflow` mode | Queued hypotheses wait only in workflow tracks that no longer fit the current science revision; `details` names each track and why. | Fix the track (`PATCH` its workflow or producer) or register a science revision it fits. |
| `forbidden` (403) | Any | The identity cannot do this: an agent claiming `workflow`, an experimenter claiming `agent`, a token created without a browser session, a missing role, a missing `write` scope. | Use the identity the [token table](#the-model-in-one-screen) gives. |
| `conflict` (409) | Job claim, track, claim | No job waits for this tester or evaluator name (or policy revision), or a genuine conflict (track paused or switched during a claim, a control a step cannot stage). | For jobs: the service account's name must equal the science revision's `tester.id` or `evaluator.id`, and the policy file's revision its `evaluator.revision`. |
| `stale_lease` (409) | Attempt or job calls | The lease token or generation is not current, it expired, or a runner-driven attempt passed its deadline. | Stop working on it. In agent mode the attempt fails for review; in workflow mode it is requeued. |
| `stale_revision` (409) | Track or draft changes | `expected_revision` is not the current one. | Read the record again and retry. |
| `validation_failed` (422) | Any document | A field breaks a schema or a rule; `details` gives JSON Pointers. A producer or experiment manifest declaring a held-out labels dataset is refused here. | Fix the document. |
| `held_out_labels_to_producer`, `held_out_labels_to_experiment` | Runner failure | A pinned producer or experiment step declares a held-out labels dataset. | Register a manifest that does not, and a science revision that marks the dataset correctly. |
| `invalid_step_output` | Runner failure | An output does not match its interface or its validator rejected it. Blamed on the candidate only for a producer output the API itself refused. | Read the reason (step, output, file, JSON Pointer) and the `validator_log`. |
| `invalid_output` | Runner failure | An output holds a symbolic link; is too large or has too many files for the Kubernetes copy-back; an experiment output has a file in a subdirectory (experiment outputs are flat files only); the scorer's `evidence` is not exactly one JSON file holding one JSON object; or the API refused an output's upload grant for a limit (its size, for example). | Write plain files directly under `/cr/outputs/<name>/`, within the limits; read the reason. |
| `missing_output` | Runner failure | A declared output was not written. | Write every declared output. |
| `step_failed`, `setup_failed` | Runner failure | The step or its setup exited non-zero or ran out of memory. | Read `step_log` or `setup_log`. |
| `deadline_exceeded` | Runner failure, sweep | A step, setup or attempt passed its deadline. | Raise `activeDeadlineSeconds` (within `max_deadline_seconds`), or make the step faster. |
| `runner_error` | Runner failure | The runner could not run the step: GitHub has no such commit or the credential cannot see it, an image cannot be pulled, a Pod stayed Pending, too many GPUs asked, a second runner on the same cache root. | Read the reason; fix the commit, image, credential or capacity. A bad commit fails every rerun: register a new manifest revision. |
| `invalid_code`, `code_not_allowed` | Runner failure | An unsafe archive or a missing `code.path` or key file; a repository outside `--github-allowed-repos`. | Fix the manifest or the runner's allowlist. |
| `evaluator_error`, `policy_mismatch` | Evaluation failure | See [Eval stage](#eval-stage). | Read the reason and the policy step's `step_log`. |
| `invalid_submission` | Submission | The claimed sheet was rejected; the attempt failed and awaits review. | Validate the sheet before submitting; a researcher `retry` requeues. |
| `upload_expired` (409) | Upload | The upload grant expired before its bytes were finished. | Request a new grant. |
| `invalid_content` (422) | Job upload | A job output named against an interface does not match it. | The tester reports `invalid_step_output` for the step. |
| `store_unavailable` (503) | Upload, download | The object store failed; nothing changed. | Retry after a pause, a bounded number of times (the runner tries 4 times). |
| `no_evaluator` | Attempt failure | The pinned science revision has no evaluator (built-in gates). | Close it; register a science revision with an `evaluator`. |
