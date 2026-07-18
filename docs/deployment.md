# Container and Kubernetes deployment

Spurfire publishes the lobby control service as a multi-architecture OCI image and the deployment chart as an OCI Helm artifact. Gameplay traffic remains peer-to-peer, and the control-plane workload never joins a lobby tailnet.

> **Public real activation is closed.** Safe deployment groundwork adds an independent kill switch fixed off and a singleton real-lobby policy. Inspection timing values remain deferred and are not rendered until a production scheduler consumes them. The chart deliberately rejects enabling real mutations and still does not supply complete lobby-route capability/abuse controls, a dynamic encrypted child vault, startup reconciliation, or a private operator listener. Ottawa remains public dry-run. See [control-plane-network-view.md](control-plane-network-view.md) for the authoritative gates and runbook.

## Artifact coordinates

After the `Packages` GitHub Actions workflow publishes a commit:

- image: `ghcr.io/rajsinghtech/spurfire-server`
- chart: `oci://ghcr.io/rajsinghtech/charts/spurfire-control`

Only GitHub Actions publishes these artifacts. Local commands in this document build, render, pull, or verify; they never push. The publishing job alone receives `packages: write` and uses `GITHUB_TOKEN`, not a PAT or any Tailscale credential.

### Image tags

A `main` push publishes:

- `main`
- `sha-<full-commit-sha>`

A stable tag such as `v0.1.0` publishes `v0.1.0`, `0.1.0`, `0.1`, `latest`, and the SHA tag. A prerelease such as `v0.2.0-rc.1` publishes only the exact prefixed tag, the unprefixed version, and the SHA tag; it never advances stable aliases.

Release tags must be strict `vX.Y.Z` or `vX.Y.Z-prerelease`, point to a commit on `main`, and match all of these files:

- `crates/spurfire-server/Cargo.toml`
- `charts/spurfire-control/Chart.yaml` `version`
- `charts/spurfire-control/Chart.yaml` `appVersion`
- `docs/release-notes-<version>.md`

Image labels record the immutable source URL, package version, full revision, commit timestamp, license, and documentation URL. The multi-platform publication includes BuildKit provenance and an SBOM.

### Chart versions

A semantic release tag publishes the same semantic chart version, with no synthetic `latest` chart tag. Each `main` run publishes a unique prerelease chart version shaped like:

```text
0.1.0-main.<run-number>.<run-attempt>.sha-<12-character-sha>
```

A main-channel chart defaults to its corresponding immutable `sha-<full-commit-sha>` image tag. Use a semantic release version for stable deployment and record the resolved image and chart digests in GitOps.

## Run the image safely

Build a local image without publishing:

```sh
docker buildx build --platform linux/amd64 --load \
  --tag spurfire-server:local .
```

Run only in dry-run mode unless real provisioning has been deliberately configured:

```sh
docker run --rm \
  --read-only \
  --cap-drop ALL \
  --security-opt no-new-privileges \
  --tmpfs /tmp:rw,noexec,nosuid,size=16m \
  --tmpfs /var/lib/spurfire:rw,noexec,nosuid,size=16m \
  -e SPURFIRE_DRY_RUN=1 \
  -e SPURFIRE_REAL_MUTATIONS_ENABLED=0 \
  -e SPURFIRE_PROVISIONING_MODE=dry_run \
  -p 127.0.0.1:8080:8080 \
  spurfire-server:local
```

The runtime image is Alpine-based, runs as UID/GID `10001`, and contains only CA certificates, BusyBox utilities, the server binary, and the license. `/healthz` returning HTTP 200 is a liveness signal; readiness must also require `"provisioning_ready":true`.

## Install the Helm chart

The defaults are intentionally safe: one replica, `Recreate`, dry-run, `SPURFIRE_REAL_MUTATIONS_ENABLED=0`, a schema-locked one-real-lobby policy, deferred (non-rendered) inspection timing values, `emptyDir`, ClusterIP, and no public HTTPRoute. This chart revision rejects setting the real-mutation switch to true.

```sh
helm upgrade --install spurfire \
  oci://ghcr.io/rajsinghtech/charts/spurfire-control \
  --version 0.1.0 \
  --namespace spurfire --create-namespace \
  --set fullnameOverride=spurfire
```

For an immutable deployment, pin the image digest:

```yaml
image:
  tag: ""
  digest: sha256:<verified-image-digest>
```

`image.digest` overrides `image.tag`. Keep the chart version pinned independently.

### Public Gateway API route

The chart includes opt-in values for the existing public Gateway and hostname:

```yaml
fullnameOverride: spurfire
httpRoute:
  enabled: true
  parentRefs:
    - group: gateway.networking.k8s.io
      kind: Gateway
      name: public
      namespace: home
  hostnames:
    - spurfire.rajsingh.info
  path:
    type: PathPrefix
    value: /
```

Gateway API CRDs and a listener that permits cross-namespace routes must already exist. TLS and DNS are responsibilities of that Gateway deployment. Chart validation permits this public route only with credential-free dry-run and the real-mutation switch off.

**Security boundary:** `X-Spurfire-Player-Id` is currently a client assertion, not authentication. Public routing is disabled by chart default. A public Gateway may serve only the static shell and forced-dry-run API; `/v1/operator/*` must use a separate private listener. Generic external authentication alone does not satisfy the exact-lobby capability, uniform-404, rate-limit, and privacy gates.

## Prototype real provisioning mode (activation blocked)

The activation-closed chart can render non-dry provider staging only with all of the following necessary prototype settings. It still emits `SPURFIRE_REAL_MUTATIONS_ENABLED=0`, forbids a public HTTPRoute in that configuration, and therefore cannot perform real mutations. These settings are not sufficient for public activation:

1. an existing Kubernetes Secret with keys `TS_CLIENT_ID` and `TS_CLIENT_SECRET`;
2. `config.dryRun=false` and a real provisioning mode;
3. persistent `ReadWriteOnce` storage for `/var/lib/spurfire`;
4. exactly one server process.

The chart accepts only the parent organization OAuth Secret name and key names; it has no values for credential payloads. Provision that parent credential through an approved external secret path, enable Kubernetes encryption at rest, and restrict who can read it. This static Secret is not an acceptable destination for one-time dynamically generated child OAuth material.

```yaml
config:
  dryRun: false
  realMutationsEnabled: false
  maxActiveRealLobbies: 1
  provisioningMode: tailnet_per_lobby

tailscale:
  existingSecret: spurfire-tailscale-oauth
  clientIdKey: TS_CLIENT_ID
  clientSecretKey: TS_CLIENT_SECRET

persistence:
  enabled: true
  storageClass: ceph-block-replicated
  accessModes: [ReadWriteOnce]
  size: 1Gi
  retain: true
```

The PVC contains non-secret JSON state and needs a writable directory because state updates use a sibling temporary file followed by an atomic rename. The parent OAuth Secret is not checksummed into the pod template. After rotating it, perform an explicit controlled restart.

Do not apply this example to Ottawa or another public listener. Safe-groundwork server revisions recognize the independent switch and reject real create/mint/delete while it is false; the chart pins it false and rejects true. Older binary/chart revisions without this contract are unsuitable. A future activation change may make `true` renderable only after every other gate is attested; credentials and non-dry values never suffice.

Do not restart or upgrade the current process-local-vault prototype while child lobbies are active. After restart, retained child-backed lobbies fail closed with `cleanup_pending` and may require exact-ID manual remediation. Production requires a dynamic encrypted child vault with workload identity/audit/backup/CAS/deletion, mutation-closed startup reconciliation across store/vault/lease/exact upstream IDs, and proven create crash-window handling. Dynamic child credentials must never enter JSON, a static Kubernetes Secret, SOPS, rendered Helm output, logs, or metrics. Shared-tailnet mode separately remains blocked until its required scopes are live-verified and still consumes the one-real-lobby quota.

Ottawa's required GitOps posture remains `dryRun=true`, `provisioningMode=dry_run`, `existingSecret=""`, and `persistence.enabled=false`. Changing it requires a separate review after every activation gate is green.

## Verify published artifacts

The workflow keyless-signs both digests with GitHub OIDC. Verify a recorded digest with the workflow identity that published it:

```sh
IDENTITY='https://github.com/rajsinghtech/spurfire/.github/workflows/packages.yml@refs/tags/v0.1.0'
ISSUER='https://token.actions.githubusercontent.com'

cosign verify \
  --certificate-identity "$IDENTITY" \
  --certificate-oidc-issuer "$ISSUER" \
  ghcr.io/rajsinghtech/spurfire-server@sha256:<image-digest>

cosign verify \
  --certificate-identity "$IDENTITY" \
  --certificate-oidc-issuer "$ISSUER" \
  ghcr.io/rajsinghtech/charts/spurfire-control@sha256:<chart-digest>
```

Pull or inspect without publishing:

```sh
docker buildx imagetools inspect \
  ghcr.io/rajsinghtech/spurfire-server@sha256:<image-digest>

helm show chart \
  oci://ghcr.io/rajsinghtech/charts/spurfire-control \
  --version 0.1.0
```

The GitHub repository should protect `main` and `v*` tags, require package validation, restrict the release environment, grant package administration only to this repository, and make GHCR packages public only when anonymous pulls are intended.

## Validation

These checks do not publish. For the control-network workstream, do not build on the development Mac: run the credential-free checks from a clean checkout on `ssh ubuntu@raj-builder`, and use GitHub Actions for cross-platform checks/artifacts. Never copy `.env` or credentials to the builder.

```sh
scripts/check-packaging.sh
cargo +1.91.0 fmt --all --check
cargo +1.91.0 clippy --locked -p spurfire-server --all-targets -- -D warnings
cargo +1.91.0 test --locked \
  -p spurfire-server -p spurfire-control -p spurfire-protocol
```

See [lobby-service.md](lobby-service.md) for current/target routes and environment behavior. See [control-plane-network-view.md](control-plane-network-view.md) for audiences, the never-join decision, activation gates, exact cleanup proof, and operator response.
