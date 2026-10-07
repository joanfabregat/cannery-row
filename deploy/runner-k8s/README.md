# Runner on Kubernetes

This kustomize base runs `cannery runner --launcher kubernetes` as a **tester only**. It is a deployment template, not an apply-ready installation: the application image is deliberately unpullable until you replace the placeholder in `kustomization.yaml` with the digest of the `ghcr.io/joanfabregat/cannery-row` image you have reviewed. The image is distroless; shell commands run only in the separately pinned BusyBox credential utilities and transfer Pods. How the launcher runs steps, and every option, is in [docs/deploy.md](../../docs/deploy.md#the-runner-on-kubernetes).

Before deploying, prepare an overlay with a dedicated namespace, matching Namespace resource and RoleBinding, a unique stable `RUNNER_ID`, the actual `API_URL` and `PROJECT`, the image digest and the cluster's verified Pod/Service CIDRs. Never install this runner into a namespace containing the API, database or unrelated workloads: its Role can create and delete Pods and PVCs there. Keep one replica with `Recreate`; two runners sharing an ID must not overlap.

Create `cannery-runner-token` from a private token file, never a literal token argument. Provide `cannery-runner-data`, readable by UID 10001, containing `datasets/<id>/<revision>/` and `baselines/<id>/<revision>/`. Populate it with a utility Pod you trust; the `cannery` image has no `kubectl cp` utilities. Review storage capacity, node placement and ReadWriteOnce scheduling for runner data and per-job volumes.

## Explicit Kubernetes identity

The runner requires explicit namespace, API URL, bearer file and CA configuration. The base supplies its namespace through the Downward API, the API address `https://kubernetes.default.svc`, and private files in `/run/cannery`. Replace the address and matching `--k8s-api-service-host` / `--k8s-api-service-port` if the cluster uses another address. The namespace-policy acknowledgement is explicit; it is an operator assertion, not evidence that the CNI enforces the supplied NetworkPolicies.

Automatic ServiceAccount mounting is disabled. A projected bound token (requested lifetime one hour, API audience selected by the cluster) and `kube-root-ca.crt` are mounted only into the credential utilities. The init container performs the first copy before the runner starts. The bounded sidecar repeats atomic owner-only copies every 15 seconds on a memory volume; the runner mounts that volume read-only and the Kubernetes client rereads its token file. Step and transfer Pods receive neither credential volume nor an automatically mounted ServiceAccount identity.

Utility copy errors terminate the utility without printing contents; Kubernetes restarts the sidecar. Monitor its restart count and file freshness. A stale Kubernetes token eventually expires and requests fail closed. The runner loads its Cannery tester credential at startup, so updating that Secret also requires a controlled runner restart. This is different from the dynamically reread Kubernetes credential. Verify token replacement and subsequent authenticated API calls on your cluster before relying on rotation.

Both utility containers run as UID/GID 10001 with group-readable projected inputs, no capabilities, no privilege escalation and a read-only root. Only copied files are mode 0600. No token appears in command arguments or logs. Do not mount `/secret` or `/identity` in the runner or generated step Pods.

## Complete tester and evaluator flow

Mount a reviewed `runner.toml` and stock policy at `/etc/cannery`. Replace the runner arguments with:

```yaml
args: [runner, --config, /etc/cannery/runner.toml, --k8s-namespace-policy-acknowledged]
```

The operator policy flag remains necessary with `--config`. Use literal values in TOML; Kubernetes does not expand ConfigMap file contents:

```toml
api_url = "https://cannery.example.org"
project = "pilchards"
data_root = "/data/runner"
work_root = "/work"
cache_root = "/work/code-cache"

[launcher]
type = "kubernetes"
runner_id = "pilchards-1"
k8s_namespace = "REPLACE_WITH_RUNNER_NAMESPACE"
k8s_api_url = "https://kubernetes.default.svc"
k8s_token_file = "/run/cannery/kubernetes.token"
k8s_ca_file = "/run/cannery/kubernetes-ca.crt"

[[kinds]]
name = "tester"
kind = "test"
token_file = "/run/cannery/tester.token"

[[kinds]]
name = "evaluator"
kind = "eval"
token_file = "/run/cannery/evaluator.token"
policy = "/etc/cannery/policy.json"
```

Create a separate evaluator Secret and mount it only into both credential utilities. Extend `refresh()` in `token-copy.sh` with an atomic copy to `evaluator.token`; do not reuse the tester credential. The evaluator account ID and policy revision must match the pinned science revision. Add an experiment kind and separate experimenter credential only when a workflow track needs it.

GPU/storage options belong in `[launcher]` or explicit flags. Networked setup/steps require the deliberate `--allow-unrestricted-egress` operator flag: the supplied policy allows broad Internet access, not the manifest's destination allowlist. Start with network-free steps where feasible. See [launcher details](../../docs/deploy.md#the-runner-on-kubernetes).
