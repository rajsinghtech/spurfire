# Asset licenses

Retrieval date: **2026-07-17 (UTC)**

This repository includes only the curated Kenney files listed below. Both source archives were downloaded directly from the official `kenney.nl` URLs, passed ZIP integrity checks, and include Kenney's license text. The archives themselves are not committed. File-level SHA-256 hashes and archive provenance are recorded in [`game/assets/kenney/manifest.json`](../game/assets/kenney/manifest.json).

## License

Both packs are released under **Creative Commons Zero v1.0 Universal (CC0 1.0)**:

- License URL: <https://creativecommons.org/publicdomain/zero/1.0/>
- SPDX identifier: `CC0-1.0`
- Kenney's included license files state that the content may be used in personal, educational, and commercial projects and that credit is not mandatory.
- The original license files are retained at:
  - `game/assets/kenney/nature-kit/License.txt`
  - `game/assets/kenney/survival-kit/License.txt`

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

## Horse model status

No horse binary was imported. The supplied verification report found horse-named files only in Kenney's **Animal Pack Redux**, which is a 2D icon pack and whose current official page/download return 404. It contains no GLB, GLTF, OBJ, rig, or animation. Because no official, currently downloadable Kenney 3D horse was verified, `game/scenes/art/horse_visual.tscn` was intentionally not created rather than substituting an unverified asset.

## Reusable scene

`game/scenes/art/frontier_prop_set.tscn` instances the selected GLBs as a categorized, reusable prop collection. It does not modify or depend on the main graybox scene.
