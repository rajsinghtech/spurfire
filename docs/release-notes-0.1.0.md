# Spurfire 0.1.0

Initial packaged control-plane release:

- `spurfire-server` multi-architecture OCI image for Linux amd64 and arm64;
- `spurfire-control` OCI Helm chart with safe dry-run defaults;
- single-replica, persistent real-mode deployment contract;
- optional Gateway API route configuration for `spurfire.rajsingh.info`;
- P2P roster display of private tailnet endpoints alongside route and RTT.

The service remains a prototype. Public identity is client-asserted, ranked-result verification is unresolved, and tailnet-per-lobby child OAuth material is process-local.
