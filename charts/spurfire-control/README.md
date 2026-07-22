# spurfire-control Helm chart

By default this chart deploys one ordinary `spurfire-server` control-plane process. Gameplay remains peer-to-peer: the workload owns provider resource lifecycle, but it does not join lobby tailnets, run a Tailscale/RustScale node, relay gameplay, or act as a gameplay witness.

The mutually exclusive `protectedAlpha` profile renders a one-container runtime (PID1 launcher plus measured worker process) and a separate private provider-broker pod/ClusterIP. It requires separate pinned runtime/broker digests, source/provenance/artifact/policy bindings, retained state, the named anti-rollback Lease, exact lobby/origin/listener, exact Kubernetes API-server CIDRs, pinned mTLS, and SOPS-provisioned mode-`0400` file mounts. It creates no public broker route. See [`docs/protected-alpha.md`](../../docs/protected-alpha.md).

Before receipt issuance, `protectedAlpha.prepare=true` performs the credential-free first phase. It creates the retained state/broker claims and unbound named Lease, then runs a no-network bootstrap Job that initializes the exact state path and emits only its instance, canonical-path, and initial-state SHA-256 bindings. Preparation is mutually exclusive with `protectedAlpha.enabled`; the owner signs those bindings and the Lease's pre-install UID/resourceVersion before admission is enabled.

## Install safely

The defaults are credential-free dry-run mode, an `emptyDir`, a ClusterIP Service, no public route, and an independent provider-mutation kill switch set to `0`:

```sh
CHART_VERSION='<reviewed-version>'
helm upgrade --install spurfire \
  oci://ghcr.io/rajsinghtech/charts/spurfire-control \
  --version "$CHART_VERSION" \
  --namespace spurfire --create-namespace \
  --set fullnameOverride=spurfire
```

Pin the image by digest in GitOps deployments:

```yaml
image:
  tag: ""
  digest: sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
```

A digest takes precedence over `image.tag`; an empty tag otherwise uses `Chart.appVersion`.

## Ordinary activation remains closed

`config.realMutationsEnabled` and `config.realAdmissionEnabled` remain fixed false. No environment value can turn the ordinary process into Alpha. Parent OAuth plus a PVC are never activation controls. The protected profile instead requires a signed, exact-artifact/store/origin/lobby/generation receipt and uses the existing one-real-lobby lease and cleanup-only recovery.

The chart schema fixes `config.maxActiveRealLobbies` at `1` as an activation policy check. It is intentionally not rendered as an environment variable because the server's singleton lease is currently fixed in code across dedicated and shared compatibility modes. Ambiguity and cleanup failure retain the lease. The cap is not a substitute for authorization.

Outside `protectedAlpha`, `config.dryRun=false` remains activation-closed private integration plumbing. Protected Alpha does not emit the ordinary real-mode booleans and may use an `HTTPRoute` only when its path is the exact signed lobby prefix.

Outside protected mode, the chart accepts only an existing parent OAuth Secret name and key names; it never accepts or renders credential values. Protected mode accepts only names of pre-existing SOPS-reconciled file Secrets. Dynamically generated child OAuth material must never enter Helm values or the runtime JSON PVC; broker custody remains encrypted in the retained broker store.

## Cached network-summary contract

The `networkSummary` values document fixed future timing bounds but are not rendered into the pod: the server does not yet parse them and no production observation/report scheduler exists. This avoids promising runtime behavior that is not wired. A future scheduler change must add parsing, boundary tests, cadence/shutdown tests, and then render the settings.

The implemented selected-lobby inspection GET reads cached state only and never triggers provider I/O, mutation, cleanup, or a user-driven poll. Explicit internal refresh calls preserve a last good value as stale after source failure; they do not synthesize offline, zero, unavailable, or absent facts. Dry-run has no provider and reports a simulated network with no tailnet DNS name/FQDN.

## Gateway API

Gateway API routing is opt-in. It is restricted to credential-free dry-run or the protected profile's exact authorized-lobby path. The supplied values use generic Gateway coordinates and the public Spurfire hostname; replace the Gateway name and namespace for your deployment:

```yaml
fullnameOverride: spurfire
httpRoute:
  enabled: true
  parentRefs:
    - group: gateway.networking.k8s.io
      kind: Gateway
      name: public-gateway
      namespace: gateway-system
  hostnames:
    - spurfire.rajsingh.info
```

Gateway API CRDs, TLS, request limits, and a listener that permits the route must already exist. This public route may serve the static inspector shell and dry-run/public-safe APIs only. Real lobby inspection requires an exact-lobby capability; operator routes require a separate private authenticated listener and must never be exposed by this `HTTPRoute`.

## Runtime hardening

Each Deployment is fixed at one replica with a `Recreate` strategy. Pods run as UID/GID `10001`, drop all Linux capabilities, disable privilege escalation, use `RuntimeDefault` seccomp and read-only root filesystems. Service-account tokens are absent by default and projected only in protected mode for exact named-Lease CAS. Startup/liveness probes check `/healthz`; readiness additionally requires `"provisioning_ready":true` because degraded health responses still return HTTP 200.

See [`docs/deployment.md`](../../docs/deployment.md) for artifact tags, signatures, publishing, and operations. Its historical real-mode examples do not override the activation-closed guard in this chart revision.

## Alpha safety configuration contract

`protectedAlpha.enabled=true` is mutually exclusive with dry run and requires dedicated-tailnet mode, two-player cap, retained runtime and broker stores, immutable runtime/broker image and supply-chain bindings, exact installation/lobby/origin/listener/API-server CIDRs, at least 900 seconds of termination grace, and separately named SOPS, TLS, and per-run MAC mounts. The runtime has no provider credential environment or mounts. Only the private broker pod receives organization credential and vault-key mounts; its ClusterIP is selected only by runtime and has no HTTPRoute. The exact route must be removed after durable `Released` evidence. Rendering this profile is not the GO decision; the immutable evidence gate in `docs/protected-alpha.md` remains mandatory.
