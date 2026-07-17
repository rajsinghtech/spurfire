# spurfire-control Helm chart

This chart deploys one `spurfire-server` control-plane process. Gameplay remains peer-to-peer. The chart fixes the workload at one replica with a `Recreate` strategy because the prototype uses a single-process JSON store and keeps child OAuth material in memory.

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

## Real mode

Create the OAuth Secret out of band with keys `TS_CLIENT_ID` and `TS_CLIENT_SECRET`. Do not place credential values in a Helm values file or Git repository. Real mode is rejected unless both an existing Secret and persistent state are configured:

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

The PVC stores only non-secret JSON state. Secret rotation deliberately does not alter the pod-template checksum; rotate the Secret and perform a controlled restart when no lobbies are active. Do not restart or upgrade a real tailnet-per-lobby deployment while lobbies exist: child OAuth credentials are process-local and restart recovery still requires manual remediation.

## Gateway API

Gateway API routing is opt-in. The supplied values target the `home/public` Gateway and `spurfire.rajsingh.info`:

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

The control API currently trusts a client-asserted player ID and does not authenticate callers. Keep `httpRoute.enabled=false` unless an external authentication and authorization policy protects the route.

## Runtime hardening

The pod runs as UID/GID `10001`, drops all Linux capabilities, disables privilege escalation and service-account token mounting, uses `RuntimeDefault` seccomp and a read-only root filesystem, and mounts only `/tmp` and `/var/lib/spurfire` writable. Startup/liveness probes check `/healthz`; readiness additionally requires `"provisioning_ready":true` because degraded health responses still return HTTP 200.

See [`docs/deployment.md`](../../docs/deployment.md) for artifact tags, signatures, publishing, and operations.
