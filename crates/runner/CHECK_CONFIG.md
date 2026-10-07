# Offline runner configuration preflight

`cannery runner --config runner.toml --check-config` loads the production configuration, reads private worker token files and evaluator policies, and validates the selected local factory inputs. Kubernetes configurations also require `--k8s-namespace-policy-acknowledged`, exactly as runtime startup does. Exit status is 0 on success and 2 for invalid configuration. `--once` conflicts with `--check-config`.

The check validates API URL safety, policy manifests, required roots, Docker/Kubernetes resource options and namespace identity, private Kubernetes tokens, configured GitHub token/App key files and provider completeness, and cache capacity conversion. It creates no clients, tasks, work directories, cache locks or claims and sends no network requests. It never prints credential contents.

A successful check is local configuration evidence, not cluster acceptance: it does not establish credentials' server-side permissions, daemon availability, image reachability, namespace policies, storage readiness, dataset presence, or a successful workflow. Optional Kubernetes CA files receive the same local regular-file and size check as factory configuration; transport trust and certificate parsing remain runtime checks.

Run the installed offline check after building the CLI:

```sh
CANNERY_NATIVE_CLI="$PWD/target/debug/cannery" \
  cargo test --frozen -p cannery-runner --test config_check \
  installed_runner_check_config_is_offline -- --ignored --exact
```

The test supplies distinct private tester/evaluator/cluster files and a real stock policy, binds an actual local TCP listener, verifies no connections arrive and no configured state roots are created, and checks missing namespace acknowledgement, invalid resources/policy, unsafe token permissions and the conflicting execution flag.
