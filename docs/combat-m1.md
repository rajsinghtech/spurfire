# M1 mounted assault-rifle combat

Spurfire M1 adds a playable, deterministic mounted-rifle range on top of the M0.5 horse controller. The rifles are fictional sidegrades with Kenney CC0 presentation models; no branded real-world weapon is represented.

## Controls

- **Mouse 1 / right trigger:** fire.
- **Mouse 2:** aim down sights (reduces native spread).
- **R:** reload.
- **4 / 5 / 6:** equip Dustwalker / Longspur / Rattler.
- **T:** reset horse (moved from R).
- Horse controls, archetype keys 1/2/3, panel H, and Escape behavior remain unchanged.

## Rifle sidegrades

- **SF-C30 Dustwalker:** balanced 30-round carbine.
- **SF-L12 Longspur:** slower precision rifle with higher per-hit damage.
- **SF-R45 Rattler:** high-cadence close-range rifle with a larger magazine and spread.

All cadence, magazine/reserve ammunition, reload duration, deterministic spread, recoil, mounted movement/yaw/airborne sway, range clamp, falloff, and headshot damage are defined in `spurfire-protocol`. `MountedWeaponController` is a thin Godot adapter.

## Authority boundary

`ShotCommand` and `ShotResult` are versioned, secret-free protocol DTOs. `CombatAuthority` validates monotonic ticks, replay/future windows, cadence, ammunition, reload state, finite normalized direction, origin leash, server range, nearest target, hit zone, damage, and elimination. Client-claimed target, zone, and damage are never authoritative.

The local M1 range uses Godot physics to supply target/zone/distance evidence to `resolve_local_hit`; native locked weapon rows calculate damage and emit confirmed results. Peer-hosted matches must instead send `ShotCommand` to the elected authority, reconstruct target geometry from authority snapshots, and call `CombatAuthority::validate_shot`.

## Presentation and assets

The course contains three target dummies, target-range dressing, crosshair, ammo/reload HUD, hit marker, muzzle flash, tracer, impacts, and rifle switching. Rifle art uses three selected models from Kenney Blaster Kit 2.1 under CC0. Exact official source, archive/file hashes, retained license, and selected files are recorded in `docs/asset-licenses.md` and `game/assets/weapons/manifest.json`.

## Validation

```bash
just game-test
```

The gate covers Rust authority vectors, Godot combat UI scenes, native class API, mounted movement/sidestep regressions, accepted native shot and ammo consumption, local authority damage reaching a target dummy, clean asset import, and a short main-scene runtime.

M1 does not yet implement Saddle Dive, rider/horse combat health, network transport, lag compensation history, or authority migration. Those are subsequent milestones built on the DTO and authority kernel added here.
