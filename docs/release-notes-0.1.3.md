# Spurfire 0.1.3

First downloadable gameplay-client release and public control-plane landing page.

## Clients

- Godot 4.7.1 exports for macOS universal, Windows x86_64, and Linux x86_64.
- Real RustScale application UDP with the idle-send wakeup fix.
- Three-peer demo support with snapshot interpolation and bounded extrapolation.
- Tab roster showing private tailnet endpoints, direct/DERP/peer-relay routes, RTT, health, and authority.
- Mounted horse locomotion, archetype sidegrades, and rifle prototype range.

## Control plane

- Public landing and download page at `spurfire.rajsingh.info`.
- Signed multi-architecture `spurfire-server` image and OCI Helm chart.
- Safe hosted control-plane preview with zero-mutation dry-run defaults.

The hosted service intentionally remains in zero-mutation dry-run mode until API authentication, authorization, and rate limiting are deployed. Gameplay visuals and animation remain prototype quality.
