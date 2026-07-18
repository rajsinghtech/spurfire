# spurfire-control Helm chart

This chart deploys one `spurfire-server` control-plane process. Gameplay remains peer-to-peer: the workload owns provider resource lifecycle, but it does not join lobby tailnets, run a Tailscale/RustScale node, relay gameplay, or act as a gameplay witness.

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

## Activation remains closed

`config.realMutationsEnabled` maps to `SPURFIRE_REAL_MUTATIONS_ENABLED` independently of `config.dryRun`, provider credentials, and `config.provisioningMode`. It defaults to `false`, and this chart revision deliberately rejects `true`. A parent OAuth Secret and a PVC are not sufficient activation controls.

A later, separately reviewed activation change must first provide all of the following:

- capability authentication and authorization for every lobby-specific route, plus abuse controls;
- a dynamic encrypted child-OAuth vault with workload identity, audit, recovery, versioning, and deletion semantics;
- mutation-closed startup reconciliation across the durable store, vault, one-real-lobby lease, and exact provider stable IDs;
- an approved create-to-vault crash-window and orphan-remediation procedure;
- a private authenticated operator listener and exact-ID cleanup alerts; and
- a separate GitOps review that keeps public routes from reaching real or operator APIs.

The chart fixes `config.maxActiveRealLobbies` at `1`. The server must apply that single lease across both dedicated `tailnet_per_lobby` and shared-tailnet compatibility modes; ambiguity and cleanup failure retain the lease. The cap is not a substitute for authorization.

`config.dryRun=false` remains renderable only as activation-closed plumbing for private, controlled integration work. It requires a pre-existing parent OAuth Secret and persistent non-secret state, continues to emit `SPURFIRE_REAL_MUTATIONS_ENABLED=0`, and cannot be combined with the chart's public `HTTPRoute`. Do not deploy that staging combination as real service activation.

The chart accepts only an existing parent OAuth Secret name and key names; it never accepts or renders credential values. Dynamically generated child OAuth material must never be placed in this Secret, Helm values, SOPS manifests, or the JSON PVC. Persistence remains opt-in and stores non-secret control records only.

## Cached network-summary contract

The chart emits the inspection timing contract below. These settings are fixed in this revision so a deployment cannot silently weaken freshness or retention semantics.

| Value | Environment variable | Seconds |
|---|---|---:|
| `networkSummary.deviceInventory.refreshSeconds` | `SPURFIRE_NETWORK_DEVICE_INVENTORY_REFRESH_SECS` | 15 |
| `networkSummary.deviceInventory.freshForSeconds` | `SPURFIRE_NETWORK_DEVICE_INVENTORY_FRESH_FOR_SECS` | 30 |
| `networkSummary.organizationPresence.refreshSeconds` | `SPURFIRE_NETWORK_ORGANIZATION_PRESENCE_REFRESH_SECS` | 60 |
| `networkSummary.organizationPresence.freshForSeconds` | `SPURFIRE_NETWORK_ORGANIZATION_PRESENCE_FRESH_FOR_SECS` | 120 |
| `networkSummary.participantReports.freshForSeconds` | `SPURFIRE_NETWORK_PARTICIPANT_REPORT_FRESH_FOR_SECS` | 15 |
| `networkSummary.participantReports.retentionSeconds` | `SPURFIRE_NETWORK_PARTICIPANT_REPORT_RETENTION_SECS` | 60 |

Background workers own collection. A selected-lobby inspection GET reads cached state only and must never trigger provider I/O, mutation, cleanup, or a user-driven poll. Source failures preserve the last good value as stale; they do not synthesize offline, zero, unavailable, or absent facts. Dry-run has no provider and must report a simulated network with no tailnet DNS name/FQDN.

## Gateway API

Gateway API routing is opt-in and is restricted by chart validation to credential-free dry-run mode. The supplied values target the `home/public` Gateway and `spurfire.rajsingh.info`:

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
```

Gateway API CRDs, TLS, request limits, and a listener that permits the route must already exist. This public route may serve the static inspector shell and dry-run/public-safe APIs only. Real lobby inspection requires an exact-lobby capability; operator routes require a separate private authenticated listener and must never be exposed by this `HTTPRoute`.

## Runtime hardening

The deployment is fixed at one replica with a `Recreate` strategy because the prototype JSON store has no HA fencing. The pod runs as UID/GID `10001`, drops all Linux capabilities, disables privilege escalation and service-account token mounting, uses `RuntimeDefault` seccomp and a read-only root filesystem, and mounts only `/tmp` and `/var/lib/spurfire` writable. Startup/liveness probes check `/healthz`; readiness additionally requires `"provisioning_ready":true` because degraded health responses still return HTTP 200.

See [`docs/deployment.md`](../../docs/deployment.md) for artifact tags, signatures, publishing, and operations. Its historical real-mode examples do not override the activation-closed guard in this chart revision.
