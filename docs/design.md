# SPURFIRE — Design Document (source of truth)

**Tagline:** *High noon. Low ping.*

## One-sentence pitch

**Spurfire** is a third-person, peer-hosted open-range shooter where every player fights from
horseback, performs dangerous flying dismounts, develops a bond with their mount, and battles
across terrain that scales with the size of the lobby.

## Core design pillars

### 1. Constant mounted movement
The horse is half of the player's combat kit. While mounted: aim/fire while trotting or
galloping, lean-shoot to either side, jump obstacles, build momentum downhill, drift/power-turn
with reins, and perform the risky **Saddle Dive**. Accuracy responds to riding skill: straight
lines over smooth ground = stable aim cone; sharp turns, jump landings, hits, rough ground =
increased sway.

### 2. The Saddle Dive
Signature mechanic. Dismount action at speed launches the rider in the chosen direction.
Airborne window: brief accuracy bonus, preserved momentum, one or more shots depending on
weapon, horse keeps running a short distance. Landing = recovery animation = vulnerable.
Scoring notifications: FLYING DISMOUNT, SADDLE DIVE HEADSHOT, FULL-GALLOP HIT, AIRBORNE REVERSAL.

## Horse system

- **Vitality:** ~2–3x rider health. At zero the horse is **spooked** (not killed), throws/drops
  the rider, and bolts. Player fights on foot until the horse returns.
- **Majestic Return:** whistle -> distant hoofbeats -> dust/silhouette -> horse gallops in from
  off-camera -> slides to a stop -> fast running mount. Base recall ~20–30s, reduced by on-foot
  actions (damage, objectives, feed pickups, hitching points, surviving under pressure).
- **Archetypes (sidegrades):** Courser (fast, fragile), Warhorse (tanky, slow accel),
  Mustang (turning/jumping/rough terrain, average speed).
- **Bond meter:** filled by jumps, clean landings, sustained gallop, near misses, mounted hits,
  returning to horse. Full meter -> **Majestic Charge** (5–7s: rapid accel, steering/traction,
  reduced sway, stagger resistance). If horse is absent, full Bond instead enables immediate
  Majestic Return.

## Provisional primary mode: Bounty Run

Score-based with respawns (not last-man-standing). 6–16 players, 12–18 min matches, highest
bounty wins. Leader becomes **Most Wanted** and is periodically revealed (map pings, flares,
circling birds, bounty bell). Scoring: elimination 100, assist 50, force horse to bolt 15,
Saddle Dive elim +25, long-range mounted hit +10, dynamic objective 50–150, Most Wanted bonus.

## World and map scaling

One large world, several combat regions; each match picks an **active territory**:

```
Playable radius = clamp(450m, 250 x sqrt(player_count), 1500m)
```

Spawns in outer 70–85% of territory, min-distance placement, validated (no spawn LOS, open
gallop ground, 2+ exits, no cliffs/deep water, similar objective travel time).

Terrain types all alter mounted tactics: open grassland, canyons, woodland/scrub, ridges,
dry riverbeds, mud/marsh, farms/ruins/mining camps, bridges/narrow passes, jump lines.
Dynamic events pull players together: ammo wagon, moving bounty target, signal tower,
supply herd, horse-buff station.

## Lobby and peer-to-peer structure

**Peer-hosted authoritative networking** (not distributed simulation):

- All gameplay machines are peers on a lobby tailnet (embedded RustScale client).
- One player elected **match authority**; validates movement, shots, damage, score, events.
- No permanent dedicated gameplay server.
- Peers keep recent state snapshots for authority migration.

Authority election inputs: direct-connection count, median/worst latency, jitter, packet loss,
upload stability, device performance, relay status.

### Lobby lifecycle

1. Player creates lobby. 2. Control service creates lobby tailnet. 3. One-use short-lived
credential per player. 4. Clients join via embedded RustScale. 5. Peers exchange version,
roster, map seed, connectivity measurements. 6. Host elected. 7. Match starts. 8. Results
submitted and verified. 9. Peers disconnect, ephemeral devices removed, tailnet destroyed.

The tailnet admin credential NEVER ships in the game client. A small backend handles
provisioning, matchmaking, cleanup, identity, leaderboard. Only real-time gameplay traffic
is peer-to-peer.

### Dependency risk

Multiple tailnets is an **alpha** Tailscale capability; one tailnet per match requires
confirmed API access/quotas/cleanup. Fallback: one managed game tailnet with lobby-specific
tags + ACLs (changes isolation model).

## Latency and connection display

Connection route is peer-specific and can change live. Labels: **Direct**, **Peer Relay**,
**DERP Relay**. Lobby shows network health summary (peers direct, median/worst RTT, authority
candidate). In-match scoreboard shows latency to authority; expanded panel shows full matrix.

## Leaderboards

In-match: bounty, elims, assists, deaths, Saddle Dive hits, horse recoveries, horse state,
authority latency, connection type. Persistent seasonal board emphasizes wins/placement;
separate style boards (longest mounted shot, longest Saddle Dive elim, airborne hits, recalls,
longest gallop, elims after losing horse). Ranked results need co-signing or a lightweight
witness/replay-validation service.

## Open questions (from design)

1. Visual setting (realistic Old West / arcade / fantasy / post-apoc)?
2. Win condition for main mode?
3. Lobby size target (provisional 6–16)?
4. Starting loadout vs found weapons?
5. Horse: persistent named companion vs per-match pick?
6. Friends-only party game vs public ranked?
7. Confirmed Tailnet Create API access?
