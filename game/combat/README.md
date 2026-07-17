# Combat presentation kit

Reusable M1 presentation scenes. These nodes never resolve hits or author damage: `CombatWeaponRig` forwards origin/direction/tick evidence to `MountedWeaponController`, while HUD, dummy, tracer, muzzle, and impact visuals consume controller signals or already-resolved endpoints.

- `mounted_rifle.tscn`, `longspur_rifle.tscn`, `rattler_rifle.tscn`: saddle/rider weapon mounts and two-frame muzzle flash.
- `target_dummy.tscn`: stable target ID, head/body zone metadata, and cosmetic confirmed-hit vitality/tip/respawn.
- `rifle_pickup.tscn`: 3 m prompt and pickup request signal. Inventory swap/ammo authority remains native/integration-owned.
- `target_range_props.tscn`: chunky original built-in-mesh range dressing.
- `combat_effects.tscn`: short-lived cosmetic tracers and impact marks from resolved endpoints.
- `../ui/combat/combat_hud.tscn`: crosshair, hit confirmation, reload progress, ammo/weapon/spread/gait card.

Headless smoke test:

```sh
godot --headless --path game res://combat/tests/combat_smoke.tscn
```
