# Integrating a research project

This is the guide to read first. It tells an agent or an engineer what to build and configure, in which order, to run a research project's experiments through Cannery Row (CR), from an empty installation to the first decided unit. It is the map and the procedure; the authoritative contracts stay in the references it links to:

| Reference | Holds |
| --- | --- |
| [spec.md](spec.md) | The system: roles, lifecycle, failure classes, trust rules. |
| [contracts.md](contracts.md) | Every document: tracks, units, uploads, evidence, jobs, step manifests, workflow tracks, the stock policy and policy steps. |
| [deploy.md](deploy.md) | The image, settings, object storage, the runner, its launchers and the stock policy. |
| [import.md](import.md) | Importing a research history kept elsewhere. |
| [deploy/runner-k8s/README.md](../deploy/runner-k8s/README.md) | The runner's Kubernetes manifests. |

The JSON Schemas under `contracts/schemas/` are the machine-checked contract; `examples/fixture/` is a complete small project that the test suite runs end to end. Every example below comes from it or follows its shape.

## The model in one screen

One installation hosts several **projects**, each fully separate: its own members, service accounts, configuration, tracks and history. Inside a project:

```text
project
├── brief (revisioned): goal, domain, constraints, conventions; every attempt pins a revision
├── science revision (immutable, versioned): metrics, datasets, interfaces, scorer, verifier, limits…
├── producers and experiment steps (registered step manifests, each revision immutable)
└── track (agent or workflow mode; planning → active once its first plan is approved)
    ├── plan (revisioned): approach + units, each a unit with its brief and context
    └── unit  #12          (plan approval) → queued → active → … → decided
        └── attempt  #12.1       claimed under a lease, then two stages:
            1. experiment  → candidate artifacts + run document              (agent, or runner's experiment kind)
            2. verify      → verification report: verified measurements,     (runner's verify kind, or an agent
                             verdict pass | fail | inconclusive               or researcher who did not run it)
            then, for the unit (documenting → deciding → decided):
            3. write-up    → summary, attempts covered, verification cited   (agent, or researcher)
            4. decision    → promote | reject | inconclusive | failed        (researcher, with a reason)
```

- The **brief** is the project's context, written once by a researcher: what the project is for, the domain, the constraints, the resources and the conventions. Every claim names the revision it runs under, and the attempt keeps it.
- A **unit** is a unit of work that an approved track plan lists. A researcher writes the plan (often with an agent) and approves it with a reason; each unit it lists is `queued`. Nothing runs before that, and CR never invents or recycles units.
- An **attempt** is one execution of a unit. It is created by a **claim**, which returns a lease token; every write on the attempt needs the current token and generation. One attempt at a time per unit.
- The stages are kept apart on purpose. The **experiment** tests the unit and produces the candidate and a **run document**: front matter with the claims (`agent_claim`, never trusted) and Markdown run notes. The **verify** job checks the frozen submission, independently of whoever ran it, and writes a **verification report**: the verified measurements (`tester_verified`), the discrepancies, and the policy's verdict with a reason. The unit is then **written up** once, by an agent or a researcher (a researcher may skip it with a reason), and a **human decision** comes last: `promote` needs a `pass` verdict; every decision needs a reason.
- A track's **mode** says only who runs the experiment stage: an outside agent (`agent`, the default) or a CR runner (`workflow`). From the submission on, both modes are identical.

Who holds which token, and what it may do:

| Identity | Token | May | May not |
| --- | --- | --- | --- |
| User, `viewer` | Personal token, or the web session | Read the project, reports, metrics, verdicts, decisions; search. | Download artifacts other than `report_asset`. |
| User, `member` | same | Viewer rights, plus comment, download artifacts and raise a concern about a track's plan. | Plan or decide. |
| User, `researcher` | same | Member rights, plus write the brief, author and approve track plans, answer or dismiss concerns, answer or escalate questions, steer agent-mode attempts, manage tracks (create, change mode, workflow or producer, pause, archive), claim in `agent` mode, claim and complete agent verify jobs of attempts it did not run, write units up or skip their write-ups, and record every human decision. | Claim in `workflow` mode; verify its own run. |
| User, installation admin | same | Create projects, grant memberships, register science and dashboard revisions, producers and experiment steps, create service accounts and their tokens. Admin is not a project role: an admin also needs a membership to act as a researcher. | |
| Service account `agent` | Service token | Claim in `agent` mode, heartbeat, upload, post the manifest, submit, release, ask questions, acknowledge answers and steering, append a transcript; claim and complete agent verify jobs of attempts it did not run; claim and complete document jobs (write-ups); raise a concern; read the project. | Claim in `workflow` mode, verify its own run, skip a write-up, comment, decide. |
| Service account `experimenter` | Service token, held by a runner only | Claim in `workflow` mode only, heartbeat, upload, submit, read the predecessor attempt's artifacts, release with a failure `code`, `step` and `logs` (trusted); raise a concern. | Plan, comment, decide, claim in `agent` mode. |
| Service account `verifier` | Service token, named like the science revision's `verify.verifier.id` | Claim runner verify jobs of its policy revision, read their inputs, upload outputs, complete or fail them; raise a concern; read the project. | Anything else on attempts or decisions. |
| Service account `decider` | Service token, named like the science revision's `decide.decider.id` | Claim decide jobs of its step revision, upload its step's logs, complete them with the decision document its step writes (an automatic decision) or fail them; raise a concern; read the project. | Any other decision; plan, comment. |

A claimed job also returns a **job lease token**, which can only read that job's inputs and write under its output prefix. Personal and service tokens are created only from a signed-in browser session (`forbidden` otherwise), so an installation needs OIDC login configured ([deploy.md](deploy.md#configuration)) before anyone can mint a token. Every token carries the scopes `read` and/or `write`; writes need `write`. Service accounts act only in their own project.

### Which interface does what

| Interface | Use it for |
| --- | --- |
| Web app | Sign in; create projects; grant memberships; create service accounts and mint every token; create and change tracks; write and review plans; write units up or skip their write-ups; decide units and failures; read everything; comment; search. |
| REST (`/api/…`) | Everything, and the only way to register science and dashboard revisions (`POST /api/projects/{slug}/config/science`), producers (`…/producers`) and experiment steps (`…/experiment-steps`). Authenticate with `Authorization: Bearer <token>`. `GET /api/me` shows who a token is. |
| MCP (`/mcp`, Streamable HTTP, same bearer token) | An agent's work: `get_brief`, `list_tracks`, `get_track`, `get_plan`, `list_plan_revisions`, `list_track_units`, `get_unit_plan`, `get_unit_history`, `search`, `claim_unit`, `heartbeat_attempt`, `create_upload`, `post_manifest`, `submit_attempt`, `release_attempt`, `metric_catalog`, `query_metrics`, `query_comparisons`, the verify and document job tools (`claim_job`, `heartbeat_job`, `get_job_input`, `create_job_upload`, `complete_job`, `fail_job`, `get_job`, `list_attempt_jobs`, `list_writeups`, `get_writeup`), the concern tools (`raise_concern`, `list_concerns`, `get_concern`), the question, steering and transcript tools (`ask`, `ask_job`, `wait_for_answer`, `get_steering`, `ack_steering`, `append_transcript`, `get_transcript`, `list_questions`, `get_question`, `list_messages`), and the researcher's `revise_brief`, the plan tools (`start_plan_revision`, `set_plan_approach`, `add_unit`, `update_unit`, `drop_unit`, `set_alignment`, `answer_concern`, `check_plan`, `submit_plan`, `review_plan`), `dismiss_concern`, `answer_question`, `escalate_question`, `post_steering`, `write_up`, `skip_writeup`, `record_decision`, `create_track`, `update_track`, `transition_track`. The brief is also an MCP resource, `cannery-row://projects/{project}/brief`, and so is each attempt's context bundle. [agents.md](agents.md) lists an agent's steps. File bytes never go through MCP: `create_upload` returns a URL the client sends them to. |
| CLI (`cannery`) | `migrate`, `serve`, `db` (dump, restore, upgrade), `runner`, `evaluator` (the stock policy alone, offline), `import`, `openapi`. |

Every error answer has the shape `{"error": {"code", "message", "details"}}`; `details` holds JSON Pointers into the request. See [Troubleshooting](#troubleshooting).

## Setting up a project

The order matters: each step is checked against the ones before it.

1. **Install and sign in.** Deploy the image and run `migrate` ([deploy.md](deploy.md#running)), with `CANNERY_AUTH_BOOTSTRAP_ADMIN_EMAILS` naming the first admin. Sign in once in the web app: users exist only after their first login.
2. **Create the project with its first track** (web app, or `POST /api/projects` with `{"slug", "title", "description", "tracks": [{"slug", "title", "description"}]}`, at least one track) and grant memberships (`PUT /api/projects/{slug}/members/{user_id}` with `{"role": "researcher"}`; `GET /api/users` finds a user id). Give yourself `researcher` if you will write the brief, create tracks or decide.
3. **Write the brief** (below). A track's first plan cannot be approved without one.
4. **Register the science revision** (below).
5. **Register producers** and, for `workflow` tracks, **experiment steps** (below).
6. **Create the other tracks**, and bind producers and workflows.
7. **Create the service accounts and their tokens.**

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

### The brief

The brief tells every agent and step what the project is for, so each unit does not have to repeat it. It is one Markdown document with YAML front matter holding `title` and a one-paragraph `goal` (schema: `GET /api/schemas/brief`); the body carries the domain, the constraints, the resources and the conventions ([the contract](contracts.md#the-brief)). A researcher writes it in the web app (**Brief**, linked from Home), with MCP `revise_brief`, or through REST:

```sh
jq -n --rawfile document brief.md '{document: $document, expected_revision: 0}' |
  curl -sf -X POST "$API/brief" -H "Authorization: Bearer $RESEARCHER" \
    -H 'Content-Type: application/json' -d @-
```

Each save is a new revision; send the current revision as `expected_revision` (`0` for the first). Agents read it with `GET $API/brief` or MCP `get_brief`, and every claim answer carries `brief` (`revision`, `sha256`, `ref`), the revision the attempt runs under; its verify jobs get the same one.

### The science revision

The science revision is the project's rules, as one immutable JSON document (`science_revision.schema.json`). Every attempt pins the revision current at its claim, so a change is a new revision and never alters running or past work. `examples/fixture/science.json` is a complete one. What an ML project puts in it:

| Field | What to put there |
| --- | --- |
| `schema_version` | `"0.2"`. |
| `verify` | Who verifies a submitted run: `{"performer": "runner", "verifier": {"id", "revision"}}`, the verifier service account's name and the policy revision its reports must name (the runner's `verify` kind claims only jobs registered to its account's name under its revision), or `{"performer": "agent"}`, an agent or a researcher who did not run the attempt. Required. See [Verify stage](#verify-stage). |
| `decide` | Who decides a written-up unit: `{"performer": "researcher"}` (the default when absent), or `{"performer": "step", "decider": {"id", "revision"}}`, the decider service account's name and the revision of its decider step (the runner's `decide` kind claims only jobs registered to its account's name under that revision). See [Document and decide](#document-and-decide). |
| `unit_fields` | A JSON Schema for a unit's `project_fields`. In a `workflow` track these are the experiment's **parameters** (learning rate, seed, model size); plan units are validated against it. |
| `metrics` | The metric registry: each `{key, unit, direction, aggregation, dimensions, splits, required_slices}`. Only registered metrics, splits and dimension values can be reported, charted or compared. |
| `datasets` | Each `{id, revision, held_out_labels, description?}`. Mark evaluation labels `held_out_labels: true` (see [the held-out labels rule](#the-held-out-labels-rule)). CR stores no dataset bytes: the runner reads them from its data root, `datasets/<id>/<revision>/`. |
| `baselines` | Controls, each `{id, revision, description?}`: immutable references a step can take as input (`from: baseline`, from `baselines/<id>/<revision>/`) and a unit can name as its `control`. Their values live in the verifier's policy, not here. |
| `interfaces` | The formats steps exchange, `{name, version, schema | format, media_type?, encoding?, max_bytes?, allow_empty?, magic?, validate?, validator?}` ([contracts](contracts.md#step-manifests-and-the-container-contract)). Every step output naming an interface is checked against it. |
| `validators` | Optional `role: validator` step manifests that an interface names in `validator`. |
| `scorer` | The project's one scorer step manifest (`role: scorer`), shared by every track so metrics are computed identically. |
| `default_producer` | `{name, revision}` of the producer a track without its own binding uses. Register it right after the revision. |
| `code_repositories` | `{"candidate": [...], "trusted": [...]}`: the GitHub repositories (`owner/name`) steps may run code from, per trust class. A class not listed runs no repository code. |
| `required_artifact_roles` | `{"attempt": [...], "verify": [...]}`: roles a submission's manifest and a verify job's output manifest must include. |
| `limits` | `resource_ceilings` (per step; a step may only ask for a resource that has a ceiling, so list `nvidia.com/gpu` for GPU steps), `max_deadline_seconds` (every step's and setup's deadline must fit it; also the time a runner verify job keeps for its policy and the whole deadline of an agent verify job, one hour when unset), `report_max_bytes` (the body of a run document or a verification report, at most 1 MiB), `max_output_bytes`. |
| `max_auto_retries` | Automatic reruns of a failed verify job (and requeues of a failed runner-driven experiment) before a human failure review. Default 1. |
| `retention`, `result_extensions` | Optional. |

The registration checks the scorer and validators against the rest of the revision (interfaces, datasets, ceilings, repositories). The dashboard revision (`POST …/config/dashboard`, [contracts](contracts.md#project-configuration-and-dashboard-contract)) is optional and separate: without one the web app derives views from the metric registry.

### The verifier and its policy

Every science revision says who verifies a run; CR applies no policy itself. Decide before registering the revision ([Verify stage](#verify-stage)):

- `runner`: a CR runner holding a `verifier` service account runs the producer and the scorer again and applies a policy file it holds:
  - the **stock policy**: a configuration file of declarative gates, such as `examples/fixture/policy.json`, whose `verifier` equals the science revision's `verify.verifier`. It holds the baseline values the gates compare against;
  - a **policy step**: a trusted step that computes the verdict, for paired bootstraps, significance tests or anything the gates cannot express.
- `agent`: an agent or a researcher who did not run the attempt verifies it by hand or with its own tools and writes the report, applying the policy the project agreed on.

With a runner, the policy file lives with the runner, not in CR: changing the policy means a new policy revision and a new science revision registering it.

### Tracks

A track is a line of research (one architecture against another). A project's first tracks come with it (`tracks` in `POST /api/projects`, created in `agent` mode with the default producer); later ones are created by a researcher (web app, REST, or MCP `create_track`) from `examples/fixture/tracks.json`-style documents:

```json
{"slug": "lexical", "title": "Lexical overlap", "description": "Ranks documents by shared words.",
 "producer": {"name": "overlap-producer", "revision": 1}}
```

- `producer` binds the producer that tests this track's candidates; omitted, `default_producer` applies. Its output interface must equal the scorer's `from: step` input interface.
- `mode` is `agent` (default) or `workflow`; a `workflow` track also names `workflow: {"steps": [{"name", "revision"}, …]}` (1 to 16 registered experiment steps, run in order). See `examples/fixture/workflow-track.json` and [the rules](contracts.md#the-workflow).
- Changing `producer`, `mode` or `workflow` is `PATCH /api/projects/{slug}/tracks/{track}` with `expected_revision`, the fields and a non-empty `reason`; attempts already claimed keep what they pinned.
- States are `planning` (new tracks: nothing is claimed until a researcher approves the first [plan](#planning-a-track)), `active`, `paused` (nothing is claimed) and `archived`, changed with `POST …/tracks/{track}/transitions` (`to_state`, `expected_revision`, `reason`).

### Planning a track

A track's plan sets its approach and the units of work it runs. It is built step by step through REST or MCP, by a researcher or by an agent working with one ([the routes](contracts.md#track-plans), [the planner's steps](agents.md#planning)); the web app's track page shows the plan, its revisions and the review, and **Edit the draft** opens the editor.

```sh
TRACK=$API/tracks/lexical
curl -sf -X POST "$TRACK/plans" -H "Authorization: Bearer $RESEARCHER"
curl -sf -X PUT "$TRACK/plans/draft/approach" -H "Authorization: Bearer $RESEARCHER" \
  -H 'Content-Type: application/json' -d '{"approach": "Establish a baseline, then vary one thing at a time."}'
curl -sf -X POST "$TRACK/plans/draft/units" -H "Authorization: Bearer $RESEARCHER" \
  -H 'Content-Type: application/json' -d @unit.json
curl -sf "$TRACK/plans/draft/check" -H "Authorization: Bearer $RESEARCHER"
curl -sf -X POST "$TRACK/plans/draft/submission" -H "Authorization: Bearer $RESEARCHER"
curl -sf -X POST "$TRACK/plans/1/review" -H "Authorization: Bearer $RESEARCHER" \
  -H 'Content-Type: application/json' -d '{"action": "approve", "reason": "Ready to run."}'
```

Approval queues each new unit and activates a `planning` track. A later revision starts from the approved one; every unit already done or in flight needs an alignment (`keep`, `obsolete` or `redo`, with a reason) before it can be submitted. The project's size limits (`GET $API/limits`) apply at each write.

Anyone working on the track, a member, a researcher, an agent or a runner step, can raise a **concern** that the plan itself is wrong (the track page's **Raise a concern**, MCP `raise_concern`, `POST $TRACK/concerns` with a Markdown document whose front matter names its `kind`, or a runner step writing `/cr/outputs/concern/concern.md`; [contracts](contracts.md#concerns)). While one is open, no new unit of the track is claimed (`409 concern_open`); work already claimed continues. Home lists the open concerns to researchers. A researcher answers one with a plan revision (**Revise the plan** on the concern opens the editor with it listed: every open concern needs an answer before the draft is submitted, and approval closes the answered ones) or dismisses it with a reason.

### Service accounts and tokens

Create one service account per identity in the project's admin settings (or `POST /api/projects/{slug}/service-accounts` with `{"kind", "name", "description"}`), then mint its token in the web app (`name`, `expires_in_days`, `scopes`; the secret is shown once):

| Kind | Name | Token goes to |
| --- | --- | --- |
| `verifier` | The science revision's `verify.verifier.id` (`cannery-runner` in the fixture). | The runner's `verify` kind. Only with performer `runner`. |
| `experimenter` | Any. | The runner's `experiment` kind, only for `workflow` tracks. Never to an agent: its failure reports are trusted. |
| `decider` | The science revision's `decide.decider.id`. | The runner's `decide` kind. Only with decide performer `step`. |
| `agent` | Any, one per agent if you want them told apart. | An outside agent working `agent` tracks, or verifying with performer `agent`. A researcher working through an agent may use a personal token instead; the agent then shows in `via`. |

Give each token `read` and `write`. A runner reads each token from its own file, with no permission for group or others (for example `600` or `400`).

## Steps

A step is one container run described by a **step manifest**, like a GitHub Actions job: a stock image pinned by digest, a script from a repository at a pinned commit, and a cached dependency install. Field names follow Argo Workflows templates. The full contract is [Step manifests and the container contract](contracts.md#step-manifests-and-the-container-contract) and [Steps as scripts](contracts.md#steps-as-scripts-code-setup-and-the-dependency-cache).

| Field (under `spec`) | Meaning |
| --- | --- |
| `role` | `producer`, `scorer`, `validator`, `experiment` or `policy`. |
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
| `trusted` | `scorer`, `validator`, `policy` | `code_repositories.trusted` | The scorer and a policy step may. |

A candidate step's setup cache never serves a trusted step, even with the same image and lockfile. The runner can narrow the repositories further with `--github-allowed-repos` ([Running the runner](#running-the-runner)).

### The `/cr` layout

| Path | Contents |
| --- | --- |
| `/cr/job.json` (read-only) | The job or attempt, the step, its manifest and, for an experiment step, the unit's `parameters`. Never a token. |
| `/cr/inputs/<name>/` (read-only) | Each declared input, SHA-256-verified before the step starts. |
| `/cr/outputs/<name>/` | Each declared output, checked against its interface after the step exits, then uploaded. |
| `/cr/code` (read-only) | With `code`: the repository tree at the commit (only `code.path`); the working directory. |
| `/cr/cache` (read-only) | With `setup`: what setup installed. |
| `/tmp` | Writable scratch. With the Docker and Kubernetes launchers `HOME` and `TMPDIR` are `/tmp`; with the local launcher `HOME` is the per-step root (`$CR_ROOT`) and `TMPDIR` is `$CR_ROOT/tmp`. |

Exit 0 is success; anything else, a deadline, an out-of-memory kill or a missing declared output is a failure. The step's stdout and stderr become its `step_log` artifact. A step has no credentials and never calls the API. With the local launcher, `/cr` is a per-step directory given in `CR_ROOT`, so write steps as `root = os.environ.get("CR_ROOT", "/cr")` to run on every launcher.

An **interface** check applies to every output that names one: presence, non-empty unless `allow_empty`, `max_bytes`, `magic`, then JSON or JSON Lines validation against `schema`, then the interface's `validator` step if any. A validator exits `0` to accept, `1` to reject, anything else when it could not check.

### The held-out labels rule

A dataset marked `held_out_labels: true` reaches only trusted judges: the scorer and a policy step. A producer or experiment step that declares one as input is refused at registration (`validation_failed`), when a track binds or names it, at claim, and again by the runner before the step runs (`held_out_labels_to_producer`, `held_out_labels_to_experiment`). An experiment step's `job.json` never lists one. The local launcher does not enforce this (its steps can read the whole data root), which is why it is for trusted fixture code only.

### The setup cache

On the first run of a key, setup runs in the step's image with only the `key_files` in `/cr/code`, an empty writable `/cr/cache`, its own network, and nothing else: no `job.json`, inputs, datasets or credentials. The key is the SHA-256 of the image digest, `setup.run`, the repository and `code.path`, the step's `env`, the setup's network, the trust class, and each key file's path and SHA-256. The commit is not in the key: a new commit with the same lockfile reuses the cache. Consequences:

- install from the lockfile only, never the project itself (`pip install .`, `npm install` of the package);
- list a setup script in `key_files`, or setup cannot see it;
- no Python virtual environment in the cache (its interpreter link points outside it): install with `pip --target`;
- variables in `env` that change per run (a seed) also change the key: keep them out of a step with a setup.

The runner caps the cache root (`cache_max_bytes`, `20Gi` by default), evicting least recently used entries.

### Worked example: a Python experiment step

An experiment that fine-tunes a model with the unit's parameters, from `example-org/ranker` (listed in `code_repositories.candidate`). The repository's `experiments/` holds `train.py` and a hash-pinned `requirements.txt` (`pip-compile --generate-hashes`).

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
      - {name: run, interface: cr-run/v0.2, path: /cr/outputs/run}
```

```python
# experiments/train.py
import json
import os
from pathlib import Path

from ranker import train  # the project's own code, next to train.py in /cr/code

root = Path(os.environ.get("CR_ROOT", "/cr"))
job = json.loads((root / "job.json").read_text())
params = job["parameters"]  # the unit's project_fields
resume = root / "inputs" / "checkpoint"  # empty on a first attempt
model = train(root / "inputs" / "train", lr=params["learning_rate"], resume=resume)
model.save(root / "outputs" / "checkpoint" / "model.safetensors")
# The run document: YAML front matter (JSON values are YAML), then the run notes.
(root / "outputs" / "run" / "run.md").write_text(
    "---\n"
    + f"artifact_roles: {json.dumps(['checkpoint'])}\n"
    + "---\n"
    + f"# Fine-tune with lr={params['learning_rate']}\n\n"
    + "Base checkpoint, train split train-r3, 3 epochs. Loss plateaued after epoch 2; "
    + "the verify job will measure retrieval quality. One seed.\n"
)
```

This needs, in the science revision: a `checkpoint/v1` interface (for example `{"name": "checkpoint", "version": 1, "format": "safetensors", "media_type": "application/octet-stream"}`), a `train` dataset not marked as held-out labels, `learning_rate` in `unit_fields`, a `nvidia.com/gpu` ceiling, and `max_deadline_seconds` of at least 14400. The track's producer then reads `checkpoint` (`from: attempt`).

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

1. **Plan.** A researcher adds the unit to the track's plan (`examples/fixture/unit.json` is a unit entry) and approves the plan ([Planning a track](#planning-a-track)); the unit is queued. Search first (`GET /api/search`, MCP `search`) for prior related work.
2. **Claim.** `POST /api/projects/{slug}/claims` with `{}` or `{"unit": 12}` or `{"track": "lexical"}` (MCP `claim_unit`). The answer holds the `attempt`, `lease_token` and `lease_generation` (send them as `X-Lease-Token` and `X-Lease-Generation` on every attempt call), `heartbeat_seconds`, and `brief`, the brief revision to read (`GET` its `ref`, or MCP `get_brief` with that `revision`). In a planned track it also names `plan`, the plan revision the attempt pinned, and `context`, the attempt's [context bundle](contracts.md#the-context-bundle) (`ref` and `bytes`): read it first.
3. **Heartbeat** `POST …/units/{number}/attempts/{sequence}/heartbeat` at least every `heartbeat_seconds` (a third of the lease TTL, `leases.ttl_seconds`, 900 seconds by default).
4. **Upload** each file: `POST …/attempts/{sequence}/uploads` with `{role, name, size_bytes, sha256, media_type}`, then send the bytes as the grant says (a `PUT upload_url`, or the `direct` presigned requests and `POST finish_url`; [the protocol](contracts.md#uploads-and-downloads)). The role is what the track's producer reads (`from: attempt`), plus every role in `required_artifact_roles.attempt`.
5. **Post the manifest** of the verified uploads (`POST …/manifest`, MCP `post_manifest`), which answers `{ref, sha256}`.
6. **Submit** the run document (`POST …/submission` with `{"document": "…"}` and an `Idempotency-Key`, MCP `submit_attempt`): Markdown with YAML front matter holding the `claims` (`agent_claim`), `provenance` and the manifest reference, and the run notes as the body ([run documents](contracts.md#run-documents)). The attempt is frozen, moves to `verifying`, and its verify job is queued.

A document the API rejects fails the attempt at once (`invalid_submission`) and opens a failure review: validate the front matter against `run.schema.json` before submitting (`GET /api/schemas/run` serves it as one self-contained schema, no token needed). A run that failed is not submitted: front matter with a `status` is refused (422) and the attempt is left as it was. To give up, or to report a failed run, `POST …/release` with a `reason` (MCP `release_attempt`): the attempt fails with `released` and a failure review case opens. An agent has no attempt deadline, only the lease.

### Questions, steering and transcripts

The claim names the working protocol as `protocol` (`GET /api/protocol` serves the text of [agents.md](agents.md), which is also the MCP server's instructions): an agent reads it once per version.

- **Questions.** An agent that needs a researcher asks under its lease (MCP `ask`, `POST …/attempts/{sequence}/questions`); a job's performer asks under the job's lease (`ask_job`). A **blocking** question puts the attempt in `waiting_on_human` and stops its lease clock; a non-blocking one states the `default` the agent proceeds on. Researchers find the open questions on Home (**Questions**, blocking ones first) and on the unit page, and answer there (**Answer**) or with MCP `answer_question`. The agent reads the answer with `wait_for_answer` or in its next heartbeat, and its lease starts afresh. A blocking question still unanswered after the project's `question_wait_seconds` (24 hours by default) releases the attempt as `unanswered_question` and queues the unit again; the question, and its answer once given, reach the next attempt's context bundle. A question pauses only its attempt: when the honest answer is that the plan is wrong, **Raise as a concern** (`escalate_question`) turns it into a [concern](#planning-a-track), which holds the whole track.
- **Steering.** A researcher posts a note to a running attempt from its attempt page (**Steering**) or with `post_steering`. The agent gets it in its heartbeat or with `get_steering` and acknowledges it with `ack_steering`; until then the page marks it **not acknowledged yet**.
- **Transcripts.** The agent appends its transcript as it works (`append_transcript`, events of [`transcript.schema.json`](../contracts/schemas/transcript.schema.json)), up to the project's `transcript_max_bytes` (64 MiB by default). The attempt page's **Open the transcript** shows it as a timeline that refreshes while the attempt runs, with tool calls collapsed and the questions, answers and steering highlighted; submission seals it as the attempt's `transcript` artifact. Cannery Row stores what it is given: the agent redacts secrets, credentials and personal data before appending.

### Workflow mode

A runner's `experiment` kind runs the experiment with an experimenter token. Setting it up takes four things ([workflow tracks](contracts.md#workflow-tracks)): `unit_fields` in the science revision describing the parameters, registered experiment steps, a `workflow` track naming them, and a runner with an `experiment` kind.

- The workflow's last step, and only it, outputs `run` (`cr-run/v0.2`): one Markdown file, `run.md`, whose front matter may hold `claims` (`agent_claim`), `artifact_roles`, `extensions` and `provenance`, and whose body holds the run notes. The runner adds the science revision and the manifest. Every other output becomes attempt artifacts whose role is the output's name (flat files only), and together they must cover `required_artifact_roles.attempt` and every `from: attempt` input of the track's producer.
- The unit supplies the parameters: the `project_fields` of its approved revision arrive as `parameters` in the claim's `workflow` object and in each step's `/cr/job.json` ([job.json](contracts.md#jobjson-of-an-experiment-step)). A plan unit sets them as `parameters` (`examples/fixture/workflow-unit.json`, `"parameters": {"top_k": 2}`), and the plan is approved as in any track.
- A `from: attempt` input of an experiment step reads the **predecessor** attempt's verified artifacts of that role (an empty directory on a first attempt), so a step can resume from a failed attempt's checkpoint. The step decides whether a partial predecessor output is usable.
- The runner claims with `{"mode": "workflow"}`, heartbeats, runs the steps in order through its launcher, checks each output against its interface and validator, uploads every output and log, posts the manifest and submits. The attempt's deadline is pinned at claim: the sum of the steps' deadlines (setups and validators included) plus `leases.job_overhead_seconds` (300 by default).
- A claim skips a track whose workflow or producer no longer fits the current science revision. When only such tracks have queued units the claim answers `409 workflow_unavailable` naming them; the runner logs it and keeps polling until a researcher fixes the track or the science revision.

### Crashes, lease loss and requeue

| What happens | Agent mode | Workflow mode |
| --- | --- | --- |
| The worker stops heartbeating (crash, lost network) | The sweep fails the attempt with `lease_expired`, an agent-side failure: a failure review case opens. A researcher's `retry` queues the unit again; the next claim creates a new attempt linked to the failed one. | The sweep fails it with `lease_expired`, a failure of the run: the unit is queued again automatically, with no review case, while `max_auto_retries` allows; then a failure review case. |
| The attempt passes its deadline | No deadline. | The API refuses the lease (`stale_lease`) and the sweep fails it with `deadline_exceeded`, then as above. |
| A late call after the lease was lost | `409 stale_lease`: stop working on the attempt. | The runner stops the step and reports nothing. |
| A blocking question waits longer than `question_wait_seconds` | The sweep fails the attempt with `unanswered_question`: the unit is queued again at once, without a review case or counting against `max_auto_retries`. | A workflow attempt asks no questions. |
| The worker gives up | `release`: fails with `released`, review case. | The runner releases with a `code` (`step_failed`, `setup_failed`, `invalid_step_output`… [the list](contracts.md#failures-release-codes-and-automatic-retries)), the failing `step` and its `logs`; requeued automatically as above. |
| An upload the API refused or could not verify, or a run document it rejected | `upload_verification_failed` or `invalid_submission`: the candidate's failure, review case at once. | The same: these are blamed on the candidate in both modes. |
| The runner gets `SIGTERM` | | It cancels the run, removes its containers, reports nothing and exits 143; the lease expires and the sweep requeues the unit. |

## Verify stage

Submission moves the attempt to `verifying` and queues one verify job ([verify jobs](contracts.md#verify-jobs)). Its performer comes from the pinned science revision's `verify`. Either way, the job's inputs are the run document's front matter (`GET …/jobs/{id}/inputs/run`, staged as `claimed.json`), the run's verified artifacts (`inputs/manifest`, `inputs/object`), and, with the claim, the unit, the brief and the plan; the run notes are not an input. It completes with one **verification report** ([contracts](contracts.md#verification-reports)): Markdown whose front matter holds `verdict` (`pass`, `fail` or `inconclusive`), `reason`, `policy_revision`, `gates` (each `pass`, `fail` or `unknown`), `measurements` (each `authority: "tester_verified"`, finite values, registered metric, split and dimensions, a `missing_reason` instead of a value when it could not be measured), `discrepancies` with the claims, optional `comparisons`, and `provenance` (`source_revision`, `science_revision`, `dataset_revision`, `control_revision` when the unit names a control), and whose optional body holds what the verifier observed (schema: `GET /api/schemas/verification`). A required slice `{dimension: value}` is covered only by a measurement whose `dimensions` are exactly that pair.

CR checks the report: the schema, the pinned provenance, the metric registry and required slices, that a `pass` reports every gate as passed, that every comparison cites the report's own verified measurements, that a runner's report comes from the registered verifier under the registered revision, and the output manifest against `required_artifact_roles.verify`. It then indexes the verified measurements and comparisons and marks the attempt `verified`: every completed verification, whatever its verdict, sends the unit to be [written up and decided](#document-and-decide).

### With the runner

The runner's `verify` kind claims the job with the verifier token, naming its policy revision (`{"phase": "verify", "revision": …}`), and runs a chain of steps:

1. the track's **producer** (candidate code, the only step of the job that executes the candidate): it reads the submission's artifacts (`from: attempt`) and datasets that are not held-out labels, and writes an intermediate output naming an interface, for example ranked results or predictions per example;
2. any **validator** that output's interface names;
3. the project's **scorer** (trusted): it reads the producer's output (`from: step`, the same interface), the held-out labels (`from: dataset`) and the frozen `claimed_sheet` (`from: attempt`, the run document's front matter, staged as `claimed.json`), and writes `evidence` (`cr-evidence/v0.2`: the report's `provenance`, `measurements`, `discrepancies` and, as `observations`, its body), plus outputs such as `per_query_results`;
4. the **policy**: the stock gates in the runner process, or a policy step through the launcher. It gives the verdict, the gates, the comparisons and the reason.

The runner composes the report, checks it as the API would and completes the job with it and a manifest of every step's outputs and logs.

Register producers with `POST /api/projects/{slug}/producers`; each registration of a name is its next immutable revision, so a new producer for a new track never mints a science revision. The scorer lives in the science revision so every track is scored the same way.

| | Stock gates | Policy step |
| --- | --- | --- |
| `policy` file | A stock configuration: `{schema_version, verifier {id, revision}, gates, baselines, default_control?}` (`examples/fixture/policy.json`, [contracts](contracts.md#verification-policy-and-the-stock-policy)). | A policy step file: exactly `{schema_version, verifier {id, revision}, step}` (`examples/fixture/policy-step.json`, [contracts](contracts.md#policy-steps)). |
| Runs | In the runner process, after the scorer. | Through the launcher, after the scorer, as a trusted step: `role: policy`, `network: none` for the step (its setup may have network), exactly one output named `verdict`, no `from: attempt` input, `code.repo` in `code_repositories.trusted`. It may read the producer's and the scorer's outputs (`from: step`, the scorer's `evidence` as `cr-evidence/v0.2`), datasets (held-out labels included) and baselines. |
| Writes | The gate results it computes. | One JSON file in `/cr/outputs/verdict/`, at most 1 MiB, with exactly `gates`, `comparisons` (optional), `verdict` and `reason`. |
| Alone | `cannery evaluator --config policy.json`, offline, for a verifier that runs its own steps. | Only as a `verify` kind of `cannery runner`. |

The job pins the unit's `parameters` when it is created (the `project_fields` of the revision the attempt pinned, `{}` without any), so a later revision never changes what a run or rerun sees: a policy step reads them from `/cr/job.json` ([contracts](contracts.md#policy-steps)) and the stock gates ignore them.

**The deadline budget.** A runner job's deadline is the sum of its steps' deadlines (setups and validators included) plus `leases.job_overhead_seconds` plus the science revision's `limits.max_deadline_seconds` (one hour when unset) for the policy. A policy step's setup deadline (when it has a setup) plus its `activeDeadlineSeconds` plus the 30 seconds the runner keeps to upload and report must fit that allowance, or the step never starts and the job fails with `deadline_exceeded`.

To switch policy revisions without stopping verification, register the new science revision, run a second `verify` entry with its own `name` and the new policy (the same token), and remove the old entry once no job of the old revision is pending.

### With an agent

With `{"performer": "agent"}`, an `agent` service account or a researcher claims the job (`POST /api/projects/{slug}/jobs/claims` with `{"phase": "verify"}`, MCP `claim_job`), never one of an attempt it ran itself, and follows [the agent's steps](agents.md#verify): read the inputs, check the claims against the artifacts, heartbeat, upload any outputs (`create_job_upload`), and complete with the report (`complete_job`). An invalid report is refused (422) with the details and the job keeps its lease, so the agent can correct it. The job's deadline is the science revision's `limits.max_deadline_seconds`.

### Failures

A failure of the verify job (a step exits non-zero, a deadline, a validator rejection, an invalid scorer output, a policy that gives no valid verdict, an invalid report from a runner, a lost lease) is an infrastructure failure: the job reruns on the same frozen submission while `max_auto_retries` allows, from the failed step, reusing the verified outputs of the steps before it; then a failure review opens. The exception is a producer output the API itself refused against its interface (`invalid_step_output`): that is the candidate's failure, reviewed at once, without a rerun. A researcher's `retry` on a verify failure queues a fresh verify job from the run.

- `policy_mismatch`: the job names another verifier id or revision than the policy file's. The claim hands out only the verifier's own revision, so this signals a misconfiguration;
- a policy step that fails reports the codes of any step, with the policy step named: `step_failed` or `setup_failed` when it exits non-zero or runs out of memory, `deadline_exceeded` when it runs out of time, `missing_output` when it writes no verdict, and `invalid_step_output` when its verdict is invalid. A policy step that does not fit the science revision's allowance fails with `deadline_exceeded` before any step runs, and its reason gives the step's and the allowance's seconds.

## Document and decide

A verified attempt ends at `verified`, whatever the verdict, and its unit moves to `documenting`: one document job waits for its write-up ([contracts](contracts.md#write-ups-and-decisions)). The same happens when a researcher stops a unit after a failure (`stop` on its failure case). The web app's Home lists the write-ups to do and the decisions to take.

- **The write-up.** An `agent` service account or a researcher claims the document job (`POST /api/projects/{slug}/jobs/claims` with `{"phase": "document"}`, MCP `claim_job`) and follows [the agent's steps](agents.md#document): read the documenter's context bundle (the brief, the plan, the unit, every attempt's run document and notes, the failures with their logs, the verification reports, the comments) and complete the job with the write-up, Markdown whose front matter holds a one-sentence `summary`, the `attempts` it covers and the `verification` it cites, as the claim's `inputs` name them. In the web app, a researcher's "Write it up" claims and completes the job in one action. A researcher may instead skip the write-up with a reason; the decision then shows "No write-up: <reason>". Nothing skips a write-up automatically.
- **The decision.** The unit then moves to `deciding` and its decision case opens. A researcher records a decision document: the `outcome` (`promote`, `reject`, `inconclusive`, or `failed` for a stopped unit) citing the verification report and the write-up, and the reason as its body. A promotion needs a `pass` verdict.
- **Automatic decisions.** A science revision with `decide: {"performer": "step", "decider": {"id": "<decider service account>", "revision": "<step revision>"}}` hands each decision to a decider step instead: a decide job is queued beside the decision case, and the runner's `decide` kind (below) runs the step and records its decision document under the same rules, a promotion only on a `pass` verdict. Home names the decider on that case, and a researcher cannot decide it while the decide job waits or runs. Once the decision is recorded, the unit page shows "Decided automatically by the decider step", and a researcher may correct it with a new decision that supersedes it. A decide job that keeps failing, after the science revision's `max_auto_retries`, leaves the case to researchers. There is no other override.

## Running the runner

`cannery runner --config runner.toml` runs every kind a project needs in one process ([job kinds](deploy.md#job-kinds-and-the-configuration-file)). The flags alone run only the `verify` kind; `--config` replaces every flag but `--once`. A complete file, with the Docker launcher:

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
kind = "verify"
token_file = "/run/secrets/verifier.token"
policy = "/etc/cannery/policy-step.json"   # or a stock configuration such as policy.json
concurrency = 2                         # two verify jobs at once; size the host for the sum
poll_seconds = 30

[[kinds]]
kind = "decide"                         # only when the science revision registers a decider step
token_file = "/run/secrets/decider.token"
decider = "/etc/cannery/decider.json"   # the decider's name, its step revision and the step manifest
```

- **One token file per kind.** Each is a regular file with no permission for group or others (for example `600` or `400`); the runner checks the mode, not the owner, so the file must belong to the user the runner runs as (UID 10001 in the image). It refuses a file that grants group or others any permission, and one token given to kinds of different types. Two `verify` entries may share one.
- `concurrency` (default 1) runs that many loops of the entry; resource ceilings apply per step, so several loops add up. `poll_seconds` (default 10) is the wait when nothing is queued or a claim failed.
- Every kind runs steps, so every kind needs `[launcher]` and `data_root`. The `decide` kind runs the decider step on the decider's context bundle ([the decide kind](contracts.md#the-runners-decide-kind)) and completes the job with the decision document it writes.
- An invalid file, an unreadable token or a missing launcher or data root refuses to start with exit code 2, naming the key (`kinds[1].token_file: …`).
- `--once` runs at most one job per entry, prints each outcome (`no job waiting`, `job <id> completed`) and exits 1 if any claim or job raised. `SIGTERM` cancels running jobs, removes their containers or Pods and exits 143; the cancelled jobs are not reported and are claimed again once their lease expires.

### Launchers

| Situation | Launcher | Where it is set up |
| --- | --- | --- |
| The `examples/fixture/` project, development, CI, trusted code you wrote. | `local` (`--unisolated-local`): steps run as local processes, as the runner's user, from a fresh copy of `step_root`; images are ignored; nothing is isolated, held-out labels and token files included. | [deploy.md](deploy.md#runner) |
| Untrusted candidate code on one dedicated machine, including GPUs on a single VM. | `docker`: one container per step, non-root, read-only root filesystem, no capabilities, `network: none` enforced. Whoever reaches the Docker socket is root on the host, so the VM is the isolation boundary. | [deploy.md](deploy.md#the-runner-on-a-container-optimized-os-vm) |
| GPU steps on a COS VM. | `docker` with `docker_gpu_mode = "cos"` (`--docker-gpu-mode cos`) and `--docker-gpu-devices 0,1`: each GPU is lent to one step at a time; a step waits for free GPUs before its deadline starts. | [deploy.md](deploy.md#gpus) |
| Untrusted code in a Kubernetes cluster, scaled or shared GPU pools. | `kubernetes`: one Pod per step on a per-job volume, Pod Security `restricted`, NetworkPolicy deny-all for `network: none`. Needs Kubernetes 1.30+, a StorageClass and enforced NetworkPolicy. | [deploy.md](deploy.md#the-runner-on-kubernetes), [deploy/runner-k8s/](../deploy/runner-k8s/README.md) |
| Only the stock policy, offline, for a verifier that runs its own steps. | None: `cannery evaluator`. | [deploy.md](deploy.md#the-stock-policy) |

The shipped Kubernetes manifests run the `verify` kind with flags; to run both kinds there, mount a configuration file and copy each token as its init container does ([with a configuration file](deploy.md#with-a-configuration-file)).

### GitHub code access

Steps with `code` need the runner to fetch the commit. Public repositories need no credential. For private ones, give the runner a GitHub App with **Contents: Read-only** (`[github] app_id`, `app_key_file`, `app_installation_id`; recommended, tokens refresh) or a read-only token file (`[github] token_file`). The credential stays in the runner's memory; no step sees it. The commit must be on a branch of `code.repo` itself, not only of a fork. `allowed_repos` (`--github-allowed-repos`) narrows what the science revision allows: a step outside it fails with `code_not_allowed`. Setup: [the runner's GitHub credential](deploy.md#the-runners-github-credential).

### Cache limits

The cache root (`cache_root`, `<work_root>/cache` by default) holds code trees by `repo@commit` and setup outputs by key, capped by `cache_max_bytes` (`20Gi` by default), least recently used first. Each runner needs its own cache root. With the Docker launcher and `docker_job_root_host`, keep the cache root under `work_root`. Details: [the code and dependency cache](deploy.md#the-code-and-dependency-cache).

## Importing history

If the project has results from before CR (notebooks, reports, run artifacts in a bucket), import them once, before or beside live work, so new units can be compared with them. `cannery import --bundle DIR --project SLUG [--science FILE] [--dry-run] [--allow-missing]` loads a human-reviewed bundle of YAML or JSON files in one transaction ([import.md](import.md); `examples/import/` is a complete bundle for the fixture science revision). Always start with `--dry-run`: an imported file can never be edited afterwards.

Imported records carry `origin: imported` and a `source_ref`; their measurements have the authorities `imported_artifact` (read from an artifact) or `imported_transcribed` (copied from a document), never `tester_verified`, so they never pass for work CR tested. A finished imported attempt with no decision of its own is `unreviewed`, a state only imports use. An imported result awaiting review continues in the live workflow. Imported artifacts are references (backend `external`, URI, size, SHA-256): CR never had the bytes. Metrics queries and comparisons return imported values only on request (`authority=imported`, `origin=imported`).

An attempt may carry its retrospective report, a Markdown file under the bundle's `reports/`: it is shown on the attempt as imported history ("Retrospective report", with its author and date), never as an agent's report or as evidence.

## Checklist

From an empty CR to the first decided unit:

1. Deploy the image with OIDC and a bootstrap admin, run `migrate`, sign in. [deploy.md](deploy.md#configuration)
2. Create the project with its first track, grant `researcher` to the people who approve and decide (yourself included), and write the brief. [Setting up a project](#setting-up-a-project), [The brief](#the-brief)
3. Write the scorer, its interfaces and validators; choose who verifies (a runner with stock gates or a policy step, or an agent) and, for a runner, write its policy file. [Steps](#steps), [Verify stage](#verify-stage)
4. Register the science revision. [The science revision](#the-science-revision)
5. Register the producers (the default first) and, for workflow tracks, the experiment steps. [Verify stage](#verify-stage), [Workflow mode](#workflow-mode)
6. Create the other tracks, each with its mode. [Tracks](#tracks)
7. Create the verifier and, for workflow tracks, experimenter service accounts, named as the science revision says, and an agent account for agent tracks or agent verification; mint their tokens in the web app. [Service accounts and tokens](#service-accounts-and-tokens)
8. Put the datasets and baselines in the runner's data root at `datasets/<id>/<revision>/` and `baselines/<id>/<revision>/`. [deploy.md](deploy.md#runner)
9. Write `runner.toml`, choose the launcher, give it the GitHub credential, start `cannery runner --config runner.toml` and check it with `--once`. [Running the runner](#running-the-runner)
10. Optionally import the history. [Importing history](#importing-history)
11. Plan each track and approve the plan. [Planning a track](#planning-a-track), [Agent mode](#agent-mode)
12. Run the experiment: an agent claims and submits, or the experiment kind does. [Experiment stage](#experiment-stage)
13. Watch the verify job (`GET …/attempts/{sequence}/jobs`, the web app's attempt page).
14. The unit is written up: an agent claims its document job (`claim_job` with `{"phase": "document"}`), or a researcher writes it up or skips it from the web app's write-up page. [Document and decide](#document-and-decide)
15. A researcher decides it on its decision case (`GET /api/projects/{slug}/review-cases`, then `POST …/review-cases/{case_id}/decisions` with `review_case_id` and the decision `document`, or the web app) and records `promote`, `reject` or `inconclusive`.

## Troubleshooting

| Code | Where | Means | Do |
| --- | --- | --- | --- |
| `nothing_to_claim` (409) | Unit claim | No queued unit in an active track of the caller's mode. | Check a plan was approved, the track is `active`, and the identity matches the mode (agents claim `agent` tracks, experimenters `workflow` tracks). The runner just keeps polling. |
| `concern_open` (409) | Unit claim | Queued units wait only in tracks with an open concern about their plan; the message names the tracks. | Read the concerns on the track page: a researcher revises the plan to answer them (Revise the plan) or dismisses them with a reason. The runner just keeps polling. |
| `workflow_unavailable` (409) | Unit claim, `workflow` mode | Queued units wait only in workflow tracks that no longer fit the current science revision; `details` names each track and why. | Fix the track (`PATCH` its workflow or producer) or register a science revision it fits. |
| `forbidden` (403) | Any | The identity cannot do this: an agent claiming `workflow`, an experimenter claiming `agent`, a token created without a browser session, a missing role, a missing `write` scope. | Use the identity the [token table](#the-model-in-one-screen) gives. |
| `conflict` (409) | Job claim, track, claim | No verify job waits for this verifier name and policy revision, only jobs of attempts the caller ran itself wait (an agent or researcher never verifies its own run), or a genuine conflict (track paused or switched during a claim, a control a step cannot stage). | For a runner: the service account's name must equal the science revision's `verify.verifier.id`, and the policy file's revision its `verify.verifier.revision`. For an agent: let another identity verify the attempts it ran. |
| `stale_lease` (409) | Attempt or job calls | The lease token or generation is not current, it expired, or a runner-driven attempt passed its deadline. | Stop working on it. In agent mode the attempt fails for review; in workflow mode it is requeued. |
| `stale_revision` (409) | Track or plan changes | `expected_revision` is not the current one. | Read the record again and retry. |
| `validation_failed` (422) | Any document | A field breaks a schema or a rule; `details` gives JSON Pointers. A producer or experiment manifest declaring a held-out labels dataset is refused here. | Fix the document. |
| `held_out_labels_to_producer`, `held_out_labels_to_experiment` | Runner failure | A pinned producer or experiment step declares a held-out labels dataset. | Register a manifest that does not, and a science revision that marks the dataset correctly. |
| `invalid_step_output` | Runner failure | An output does not match its interface or its validator rejected it. Blamed on the candidate only for a producer output the API itself refused. | Read the reason (step, output, file, JSON Pointer) and the `validator_log`. |
| `invalid_output` | Runner failure | An output holds a symbolic link; is too large or has too many files for the Kubernetes copy-back; an experiment output has a file in a subdirectory (experiment outputs are flat files only); the scorer's `evidence` is not exactly one JSON file holding one JSON object; or the API refused an output's upload grant for a limit (its size, for example). | Write plain files directly under `/cr/outputs/<name>/`, within the limits; read the reason. |
| `missing_output` | Runner failure | A declared output was not written. | Write every declared output. |
| `step_failed`, `setup_failed` | Runner failure | The step or its setup exited non-zero or ran out of memory. | Read `step_log` or `setup_log`. |
| `deadline_exceeded` | Runner failure, sweep | A step, setup or attempt passed its deadline. | Raise `activeDeadlineSeconds` (within `max_deadline_seconds`), or make the step faster. |
| `runner_error` | Runner failure | The runner could not run the step: GitHub has no such commit or the credential cannot see it, an image cannot be pulled, a Pod stayed Pending, too many GPUs asked, a second runner on the same cache root. | Read the reason; fix the commit, image, credential or capacity. A bad commit fails every rerun: register a new manifest revision. |
| `invalid_code`, `code_not_allowed` | Runner failure | An unsafe archive or a missing `code.path` or key file; a repository outside `--github-allowed-repos`. | Fix the manifest or the runner's allowlist. |
| `policy_mismatch` | Verify failure | The job names another verifier id or policy revision than the runner's policy. See [Failures](#failures). | Align the runner's policy with the science revision's `verify.verifier`. |
| `invalid_submission` | Submission | The run document was rejected; the attempt failed and awaits review. | Validate the front matter before submitting; a researcher `retry` requeues. |
| `upload_expired` (409) | Upload | The upload grant expired before its bytes were finished. | Request a new grant. |
| `invalid_content` (422) | Job upload | A job output named against an interface does not match it. | The verifier reports `invalid_step_output` for the step. |
| `store_unavailable` (503) | Upload, download | The object store failed; nothing changed. | Retry after a pause, a bounded number of times (the runner tries 4 times). |
| `superseded` | Attempt failure | The attempt was waiting for a test or an evaluation when the installation moved to verify jobs. | A researcher's `retry` queues a fresh verify job from the run. |
