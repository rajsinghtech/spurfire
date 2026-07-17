# Asset licenses

Retrieval date: **2026-07-17 (UTC)**

This repository includes only the curated Kenney files listed below. All three source archives were downloaded directly from the official `kenney.nl` URLs, passed ZIP integrity checks, and include Kenney's license text. The archives themselves are not committed. File-level SHA-256 hashes and archive provenance are recorded in [`game/assets/kenney/manifest.json`](../game/assets/kenney/manifest.json) and [`game/assets/weapons/manifest.json`](../game/assets/weapons/manifest.json).

## License

All three packs are released under **Creative Commons Zero v1.0 Universal (CC0 1.0)**:

- License URL: <https://creativecommons.org/publicdomain/zero/1.0/>
- SPDX identifier: `CC0-1.0`
- Kenney's included license files state that the content may be used in personal, educational, and commercial projects and that credit is not mandatory.
- The original license files are retained at:
  - `game/assets/kenney/nature-kit/License.txt`
  - `game/assets/kenney/survival-kit/License.txt`
  - `game/assets/weapons/kenney-blaster-kit/License.txt`

## Kenney Nature Kit 2.1

- Official pack page: <https://kenney.nl/assets/nature-kit>
- Official archive URL: <https://kenney.nl/media/pages/assets/nature-kit/37ac38a37b-1677698939/kenney_nature-kit.zip>
- Source archive filename: `kenney_nature-kit.zip`
- Source archive SHA-256: `fa7974a0d342bfe63c38664ba9f8ec1a4aab8ea25f099bdc56870e33588c4d9d`
- Pack metadata in `License.txt`: Nature Kit 2.1, creation date 2020-04-29

Selected source files:

| Source path in archive | Repository path |
| --- | --- |
| `License.txt` | `game/assets/kenney/nature-kit/License.txt` |
| `Models/GLTF format/cactus_short.glb` | `game/assets/kenney/nature-kit/cactus_short.glb` |
| `Models/GLTF format/cactus_tall.glb` | `game/assets/kenney/nature-kit/cactus_tall.glb` |
| `Models/GLTF format/fence_gate.glb` | `game/assets/kenney/nature-kit/fence_gate.glb` |
| `Models/GLTF format/fence_simple.glb` | `game/assets/kenney/nature-kit/fence_simple.glb` |
| `Models/GLTF format/rock_largeA.glb` | `game/assets/kenney/nature-kit/rock_largeA.glb` |
| `Models/GLTF format/rock_smallA.glb` | `game/assets/kenney/nature-kit/rock_smallA.glb` |
| `Models/GLTF format/sign.glb` | `game/assets/kenney/nature-kit/sign.glb` |
| `Models/GLTF format/tree_default.glb` | `game/assets/kenney/nature-kit/tree_default.glb` |
| `Models/GLTF format/tree_oak.glb` | `game/assets/kenney/nature-kit/tree_oak.glb` |

## Kenney Survival Kit 2.0

- Official pack page: <https://kenney.nl/assets/survival-kit>
- Official archive URL: <https://kenney.nl/media/pages/assets/survival-kit/4065a8185b-1712149243/kenney_survival-kit.zip>
- Source archive filename: `kenney_survival-kit.zip`
- Source archive SHA-256: `c3586341b5932c87eb43d75d915434f47daed168b17ed36a03e8ca9977c7443e`
- Pack metadata in `License.txt`: Survival Kit 2.0, creation date 2024-04-03

Selected source files:

| Source path in archive | Repository path |
| --- | --- |
| `License.txt` | `game/assets/kenney/survival-kit/License.txt` |
| `Models/GLB format/barrel-open.glb` | `game/assets/kenney/survival-kit/barrel-open.glb` |
| `Models/GLB format/barrel.glb` | `game/assets/kenney/survival-kit/barrel.glb` |
| `Models/GLB format/signpost.glb` | `game/assets/kenney/survival-kit/signpost.glb` |
| `Models/GLB format/Textures/colormap.png` | `game/assets/kenney/survival-kit/Textures/colormap.png` |

The Nature Kit GLBs embed their materials. The selected Survival Kit GLBs reference the copied `Textures/colormap.png`; it is retained at the same relative path required by the source files.

## Kenney Blaster Kit 2.1

The official Kenney catalog was checked first. **Blaster Kit** provides suitable small, low-poly, generic 3D long-gun silhouettes, so no third-party source was needed or imported. The generic kit avoids branded replicas and matches the existing Kenney environment art. Archive preview images were used only to select models and are not redistributed.

- Official pack page: <https://kenney.nl/assets/blaster-kit>
- Official archive URL: <https://kenney.nl/media/pages/assets/blaster-kit/261d80a716-1753959510/kenney_blaster-kit_2.1.zip>
- Source archive filename: `kenney_blaster-kit_2.1.zip`
- Source archive size: 1,724,676 bytes
- Source archive SHA-256: `91e3093e95427d59625e7e2ce2d0399b861600160fd0b4ada7714796b67cea8c`
- Archive verification: `unzip -t` completed with no errors
- Pack metadata in `License.txt`: Blaster Kit 2.1, creation date 2025-07-31
- License: **CC0 1.0 Universal** (<https://creativecommons.org/publicdomain/zero/1.0/>; SPDX `CC0-1.0`)
- Redistribution: permitted. Kenney's included license explicitly permits personal, educational, and commercial use; attribution is optional. The original text is retained at `game/assets/weapons/kenney-blaster-kit/License.txt`.

Selected source files:

| Source path in archive | Repository path | Use |
| --- | --- | --- |
| `License.txt` | `game/assets/weapons/kenney-blaster-kit/License.txt` | Original license evidence |
| `Models/GLB format/blaster-e.glb` | `game/assets/weapons/kenney-blaster-kit/blaster-e.glb` | Dustwalker carbine silhouette |
| `Models/GLB format/blaster-f.glb` | `game/assets/weapons/kenney-blaster-kit/blaster-f.glb` | Longspur long/heavy silhouette |
| `Models/GLB format/blaster-g.glb` | `game/assets/weapons/kenney-blaster-kit/blaster-g.glb` | Rattler compact silhouette |
| `Models/GLB format/Textures/colormap.png` | `game/assets/weapons/kenney-blaster-kit/Textures/colormap.png` | Shared source texture referenced by the GLBs |

Exact file sizes and SHA-256 hashes are in [`game/assets/weapons/manifest.json`](../game/assets/weapons/manifest.json). The curated import is 215,067 bytes, well below the 15 MB limit.

Reusable Godot wrappers are in `game/scenes/art/weapons/`. Each wrapper preserves the source model, adds a restrained fictional color-identity accent (Dustwalker tan/brown, Longspur gunmetal/wood, Rattler olive), declares a `Muzzle` marker, and uses Godot's **-Z forward** convention. These scene-level accents are original project geometry; the underlying GLBs and source texture are unmodified.

## Horse model status

No horse binary was imported. The supplied verification report found horse-named files only in Kenney's **Animal Pack Redux**, which is a 2D icon pack and whose current official page/download return 404. It contains no GLB, GLTF, OBJ, rig, or animation. Because no official, currently downloadable Kenney 3D horse was verified, `game/scenes/art/horse_visual.tscn` was intentionally not created rather than substituting an unverified asset.

## Reusable scene

`game/scenes/art/frontier_prop_set.tscn` instances the selected GLBs as a categorized, reusable prop collection. It does not modify or depend on the main graybox scene.
