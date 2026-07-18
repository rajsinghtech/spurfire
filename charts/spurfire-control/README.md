# spurfire-control Helm chart

This chart deploys one `spurfire-server` control-plane process. Gameplay remains peer-to-peer, and the control plane never joins a lobby tailnet. The chart fixes the workload at one replica with a `Recreate` strategy because the prototype uses a single-process JSON store and keeps child OAuth material in process memory.

> **Public real activation is closed.** Chart schema support for a real provisioning mode is not activation approval. The current chart has no dynamic encrypted child-credential vault, startup reconciler, private operator listener, capability/rate-limit policy, singleton real-lobby lease, or independent default-off real-mutation switch. Keep public deployments forced dry-run.

## Install safely

The defaults are credential-free dry-run mode, an `emptyDir`, a ClusterIP Service, and no public route:

```sh
helm upgrade --install spurfire \
  oci://ghcr.io/rajsinghtech/charts/spurfire-control \
  --version 0.1.0 \
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

## Required public dry-run posture

A public deployment, including Ottawa, must retain all four values:

```yaml
config:
  dryRun: true
  provisioningMode: dry_run
tailscale:
  existingSecret: ""
persistence:
  enabled: false
```

With this posture, the server has no provider credential path, persists no real state, and permits no provider mutation. Ottawa currently uses these values. A public HTTPRoute does not change their meaning and is not evidence of a real deployment.

## Prototype real-mode values are not sufficient

The chart currently validates that non-dry mode names an existing parent-organization OAuth Secret and enables persistent non-secret state. That mechanism is useful only for a private, deliberately supervised prototype probe:

```yaml
config:
  dryRun: false
  provisioningMode: tailnet_per_lobby

tailscale:
  existingSecret: spurfire-tailscale-oauth

persistence:
  enabled: true
  storageClass: ceph-block-replicated
  size: 1Gi
  retain: true
```

Do **not** apply that example to Ottawa or another public listener. In the current software, credentials plus non-dry configuration can reach provider mutations; the required independent `SPURFIRE_REAL_MUTATIONS_ENABLED=false` gate does not exist yet.

The named Kubernetes Secret is only for the parent organization OAuth pair. It must be provisioned out of band through an approved secret path; credential values never belong in Helm values or Git. It is not an acceptable vault for dynamically generated child OAuth credentials.

Public real activation requires a dynamic encrypted child vault (intended to be setec-backed) with workload identity, audit, backup/recovery, CAS/versioning, and deletion semantics; mutation-closed startup reconciliation against store/vault/lease/exact upstream IDs; and the complete checklist in [`docs/control-plane-network-view.md`](../../docs/control-plane-network-view.md). Dynamic child credentials must never be rendered into a Kubernetes Secret, SOPS manifest, ConfigMap, values file, annotation, or checksum.

The PVC contains non-secret state only. The existing JSON store and `Recreate` deployment are single-writer, not HA fencing. A real deployment remains one process until a transactional/fenced store is approved. Do not restart or upgrade the current process-local-vault prototype while a real child lobby exists: restart loses cleanup credentials, fails closed, and may require exact-ID manual provider remediation.

## Gateway API

Gateway API routing is opt-in. The supplied example targets the `home/public` Gateway and `spurfire.rajsingh.info`:

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

The current API uses client-asserted player IDs rather than authentication. A public Gateway may expose only the static shell and forced-dry-run APIs. It must not expose real lobby data or any `/v1/operator/*` route.

The accepted target requires exact-lobby capabilities over TLS, gateway and application rate limits, uniform 404 anti-enumeration, no-store security headers, abuse alerting, and a separate private operator listener authenticated with mTLS/OIDC or Kubernetes port-forward identity. A public route and external generic authentication alone do not satisfy those gates.

## Runtime hardening

The pod runs as UID/GID `10001`, drops all Linux capabilities, disables privilege escalation and service-account token mounting, uses `RuntimeDefault` seccomp and a read-only root filesystem, and mounts only `/tmp` and `/var/lib/spurfire` writable. Startup/liveness probes check `/healthz`; readiness additionally requires `"provisioning_ready":true` because degraded health responses still return HTTP 200.

These controls do not authorize real mode. Before activation, chart/rendered-manifest tests must additionally prove:

- real mutations remain independently default-off;
- exactly one writer or approved fencing is configured;
- the public Service/HTTPRoute cannot reach operator routes;
- no dynamic child credential appears in rendered resources;
- encrypted vault workload identity is least-privilege and distinct from participant/operator identity;
- exact-ID cleanup, quota-lock, vault, and reconciliation alerts are wired;
- Ottawa's four dry-run values remain policy-enforced until a separate GitOps approval.

See [`docs/deployment.md`](../../docs/deployment.md) for artifact tags, signatures, publishing, and current chart operations. See [`docs/control-plane-network-view.md`](../../docs/control-plane-network-view.md) for network ownership, audiences, activation gates, reconciliation, and the operator runbook.
