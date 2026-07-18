# Container and Kubernetes deployment

Spurfire publishes the lobby control service as a multi-architecture OCI image and the deployment chart as an OCI Helm artifact. Gameplay traffic remains peer-to-peer.

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

- `crates/spurfire-server/Cargo.toml` and its `Cargo.lock` package row;
- `charts/spurfire-control/Chart.yaml` `version` and `appVersion`;
- `game/project.godot` `config/version`;
- the landing-page release-candidate source label; and
- `docs/release-notes-<version>.md` and its heading.

`scripts/check-release-metadata.sh [expected-version]` enforces that agreement and also keeps internal crate versions at 0.1.0. Image labels record the immutable source URL, package version, full revision, commit timestamp, license, and documentation URL. The multi-platform publication includes BuildKit provenance and an SBOM.

### Chart versions

A semantic release tag publishes the same semantic chart version, with no synthetic `latest` chart tag. Each `main` run publishes a unique prerelease chart version shaped like:

```text
0.1.0-main.<run-number>.<run-attempt>.sha-<12-character-sha>
```

A main-channel chart defaults to its corresponding immutable `sha-<full-commit-sha>` image tag. Use a semantic release version for stable deployment and record the resolved image and chart digests in GitOps.

### Client preflight and publication

`.github/workflows/client-release.yml` is deliberately a **nonpublishing Client Preflight**. Pull requests and manual dispatches build Linux x86_64, Windows x86_64, and macOS universal archives; a later tag run does the same. These are expiring GitHub Actions artifacts, not a release. The jobs use checksum-verified Godot 4.7.1 editors/templates, need no repository secrets, and do not require Apple notarization.

A stable client can be published only by a later explicit dispatch of `.github/workflows/client-publish.yml` for an existing tag and successful tag preflight run. That workflow verifies the tag commit, metadata, exact artifact set, and successful Ubuntu/macOS/Windows source gates plus Linux Godot smoke before creating the release. It refuses to replace a published release or its assets. Release preparation itself never tags or invokes either publishing path.

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
  -e SPURFIRE_PROVISIONING_MODE=dry_run \
  -p 127.0.0.1:8080:8080 \
  spurfire-server:local
```

The runtime image is Alpine-based, runs as UID/GID `10001`, and contains only CA certificates, BusyBox utilities, the server binary, and the license. `/healthz` returning HTTP 200 is a liveness signal; readiness must also require `"provisioning_ready":true`.

## Install the Helm chart

The defaults are intentionally safe: one replica, `Recreate`, dry-run, `emptyDir`, ClusterIP, and no public HTTPRoute.

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

Gateway API CRDs and a listener that permits cross-namespace routes must already exist. TLS and DNS are responsibilities of that Gateway deployment.

**Security boundary:** `X-Spurfire-Player-Id` is currently a client assertion, not authentication. Public routing is disabled by default. Do not expose the control API without an external authentication/authorization policy, request limits, and appropriate network controls.

## Real provisioning mode

Real mode requires all of the following:

1. an existing Kubernetes Secret with keys `TS_CLIENT_ID` and `TS_CLIENT_SECRET`;
2. `config.dryRun=false` and a real provisioning mode;
3. persistent `ReadWriteOnce` storage for `/var/lib/spurfire`;
4. exactly one server process.

The chart accepts only the Secret name and key names; it has no values for credential payloads. Provision the Secret through an external secret manager, enable Kubernetes encryption at rest, and restrict who can read it.

```yaml
config:
  dryRun: false
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

The PVC contains non-secret JSON state and needs a writable directory because state updates use a sibling temporary file followed by an atomic rename. The OAuth Secret is not checksummed into the pod template. After rotating it, perform an explicit controlled restart.

Do not restart or upgrade a real tailnet-per-lobby deployment while lobbies are active. Child OAuth pairs remain process-local; after a restart, retained child-backed lobbies fail closed with `cleanup_pending` and may require manual remediation. A production secret vault and reconciliation loop are still blockers. Shared-tailnet mode separately remains blocked until its required Tailscale scopes are live-verified.

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

## Local validation

These checks do not publish:

```sh
scripts/check-release-metadata.sh
scripts/check-packaging.sh
cargo +1.91.0 fmt --all --check
cargo +1.91.0 clippy --locked -p spurfire-server --all-targets -- -D warnings
cargo +1.91.0 test --locked \
  -p spurfire-server -p spurfire-control -p spurfire-protocol
```

See [lobby-service.md](lobby-service.md) for routes, environment variables, capability boundaries, and restart behavior.
