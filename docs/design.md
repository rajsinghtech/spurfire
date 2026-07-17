# SPURFIRE — Design Document (source of truth)

**Tagline:** *High noon. Low ping.*

## One-sentence pitch

**Spurfire** is a third-person, peer-hosted open-range movement shooter where every player
fights from horseback, performs dangerous flying dismounts, builds Spur with stylish riding,
and battles across terrain that scales with the size of the lobby.

## Identity

Fast, low-latency, Kenney-style low-poly arcade West. Movement technique is the skill
ceiling: gallop momentum, drift turns, jump lines, the Saddle Dive, the running mount, and a
compact on-foot kit. Latency is player-visible and treated as a design constraint, not an
ops metric.

## Core design pillars

### 1. Constant mounted movement
The horse is half of the player's combat kit. While mounted: aim/fire while trotting or
galloping, jump obstacles, build momentum downhill, drift/power-turn with reins, and perform
the risky **Saddle Dive**. Accuracy responds to riding skill: straight lines over smooth
ground = stable aim cone; sharp turns, jump landings, hits, rough ground = increased sway.

### 2. The Saddle Dive
Signature mechanic — and it has **no dedicated button**: dismounting at speed (>= 8 m/s)
*is* the dive, launching the rider in the chosen direction. Airborne window: brief accuracy
bonus, preserved momentum, capped shots per weapon, horse keeps running a short distance.
Landing = recovery animation = vulnerable. Airborne fire is dive-only; normal jump-air fire
stays rejected so the dive stays special. Scoring notifications: FLYING DISMOUNT, SADDLE
DIVE HEADSHOT, FULL-GALLOP HIT, AIRBORNE REVERSAL.

### 3. Simple inputs, technical mastery
Depth comes from context — speed, terrain, timing, angle — never from input complexity.
Two enforced rules:

- **No new buttons.** A new mechanic must contextually overload an existing input.
- **One-clause techniques.** Every technique must be explainable as "do X while Y"
  (dive = *dismount while galloping*; roll = *tap crouch while sprinting*; headglitch =
  *crouch behind low cover*). Milestone acceptance: a new tester executes the milestone's
  technique within 2 minutes of hearing the clause.

Control map:

| Input | Mounted | On foot |
|---|---|---|
| WASD + mouse | steer / aim | move / aim |
| M1 / M2 | fire / ADS | fire / ADS |
| Space | jump | jump |
| Shift (hold) | gait up -> gallop | sprint |
| Ctrl (hold) | drift / power-turn | crouch |
| Ctrl (tap while sprinting) | — | Tactical Roll |
| E | dismount (>= 8 m/s = Saddle Dive) | mount / running mount |
| Q | Spur spend -> Majestic Charge | whistle -> Majestic Return (instant if meter full) |
| R / 4-5-6 / Tab | reload / weapon select / scoreboard | same |

### 4. Low ping as a feature
Route class and RTT are player-visible, and netcode targets are design constraints, not ops
metrics: authority migration < 3 s with score intact, authority hit% advantage < 5%,
interpolation delay 100 ms, lag-compensation rewind capped at 150 ms.

## Visual setting

Stylized arcade West: low-poly, flat-shaded, saturated desert palette, Kenney CC0 asset
pipeline (provenance tracked in `docs/asset-licenses.md`). Weapons are fictional SF-series
sidegrades; no real-world brands. Graybox-to-final is a restyle, not a rebuild.

## Weapons and loadout

The SF-series rifle trio is the weapon identity (stat rows locked in `spurfire-protocol`):

- **SF-C30 Dustwalker** — balanced 30-round carbine.
- **SF-L12 Longspur** — slow precision rifle, high per-hit damage, headshot king.
- **SF-R45 Rattler** — high-cadence close-range rifle, big magazine, wide spread.

Starting loadout: pick one rifle at spawn and at each respawn. No ground scavenging —
looting friction fights the pace. Weapon/ammo pickups exist only as dynamic-objective
rewards. ADS reduces spread; accuracy responds to the sway stack (gait, turn, terrain,
impulses, airborne).

## Horse system

- **Vitality:** ~2–3x rider health. At zero the horse is **spooked** (not killed), throws
  the rider, and bolts. Player fights on foot until the horse returns.
- **Majestic Return:** whistle -> distant hoofbeats -> dust/silhouette -> horse gallops in
  from off-camera -> slides to a stop -> fast running mount. Base recall 20 s, reduced only
  by damage dealt (−1 s per 25 dmg) and objective ticks (−2 s), floor 8 s.
- **Archetypes (sidegrades):** Courser (fast, fragile), Warhorse (tanky, slow accel),
  Mustang (turning/jumping/rough terrain, average speed). Per-match pick.
- **Spur meter** (0–100, **no decay**), filled by exactly four sources: mounted hit +2 /
  mounted elim +6, Saddle Dive elim +8, jump +4 / clean landing +2, near miss +3. One spend
  on one input, effect depends on state: mounted -> **Majestic Charge** (6 s: rapid accel,
  steering/traction, reduced sway, stagger resistance); horseless -> **instant Majestic
  Return** (consumes the meter, no charge).

## On-foot kit

Losing your horse is a dramatic mid-match arc, not a death sentence — and on-foot play is a
real, butter-smooth kit that stays deliberately weaker than mounted play:

- **Sprint** 4.5 m/s with stamina; walk 2.0 m/s.
- **Crouch** (hold): 1.2 m/s, steadiest stance (sway ×0.8), lowered capsule and eye height.
  **Headglitching is designed, not accidental:** the course kit standardizes low cover at
  1.0–1.1 m so a crouched eye-line clears it while the body is protected. The exposed head
  is a headshot zone with multiplier damage, so the trade is real.
- **Tactical Roll** (tap crouch while sprinting): ~0.5 s, ~3.5 m displacement, no fire
  during, small sway penalty on exit, ~1.5 s cooldown, cancels (and later resumes) reload.
  **No invulnerability frames** — under peer-hosted authority, i-frames create unresolvable
  hit disputes at real RTTs; the dodge is displacement plus a crouch-height hitbox during
  the roll, which stays deterministic under authority validation.
- **Feel contract:** buffered inputs (~0.15 s), grace windows on stance transitions,
  explicit accel/decel curves per stance, and cancel windows specified in tuning tables —
  never left to animation length.

Guardrail: players should spend **>= 70% of match time mounted**; on-foot duel wins vs
mounted target 25–35%. If crouch-camping appears in playtests, the first dial is faster
recall — not kit nerfs.

## Primary mode: Bounty Run

Score-based with respawns (not last-man-standing). 6–16 players, 12–18 min matches, highest
bounty wins. Leader becomes **Most Wanted** and is periodically revealed (map pings, flares,
circling birds, bounty bell). Scoring: elimination 100, assist 50, force horse to bolt 15,
Saddle Dive elim +25, long-range mounted hit +10, dynamic objective 50–150, Most Wanted
bonus. Respawns grant +20% move speed for 10 s to kill the ride-back slog.

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
- **Mid-match authority is peer-owned:** on authority silence, every survivor recomputes
  `election_v1` over the match-start measurement matrix restricted to the survivor set —
  deterministic, coordination-free. The control plane observes and validates; it never
  decides mid-match.
- Every peer retains a 10 s state ring buffer. The authority broadcasts `MatchState`
  keyframes (2 Hz) alongside 20 Hz snapshot deltas; a migrating successor restores from
  keyframe + deltas and announces with a hash of the restored state for divergence checks.
- Shot validation uses authority-side rewind capped at 150 ms (lag compensation) with
  stance-aware hitboxes.

Authority election inputs: direct-connection count, median/worst latency, jitter, packet
loss, upload stability, device performance, relay status.

### Lobby lifecycle

1. Player creates lobby. 2. Control service creates lobby tailnet. 3. One-use short-lived
credential per player. 4. Clients join via embedded RustScale. 5. Peers exchange version,
roster, map seed, connectivity measurements. 6. Host elected. 7. Match starts. 8. Results
submitted and verified. 9. Peers disconnect, ephemeral devices removed, tailnet destroyed.

The tailnet admin credential NEVER ships in the game client. A small backend handles
provisioning, matchmaking, cleanup, and lobby lifecycle. Only real-time gameplay traffic
is peer-to-peer.

### Dependency risk

Multiple tailnets is an **alpha** Tailscale capability; one tailnet per match requires
confirmed API access/quotas/cleanup. Fallback: one managed game tailnet with lobby-specific
tags + ACLs (changes isolation model).

## Latency and connection display

Connection route is peer-specific and can change live. Labels: **Direct**, **Peer Relay**,
**DERP Relay**. Lobby shows network health summary (peers direct, median/worst RTT,
authority candidate). In-match scoreboard shows latency to authority; expanded panel shows
the full matrix.

## Identity, accounts, and leaderboards

**No accounts and no persistent user state in this phase.** Identity is ephemeral per lobby
(UUID + display name); lobby access uses a creator-shared join code on top of the service's
rate limits. Leaderboards are in-match only: bounty, elims, assists, deaths, Saddle Dive
hits, horse recoveries, horse state, authority latency, connection type. Persistent and
seasonal boards, style boards, and ranked play are deferred until ranked-results trust (D5)
is designed — the intended mechanism is a peer co-sign quorum, already recorded (not
trusted) in the results DTO.

## Settled questions (formerly open)

1. **Visual setting** -> stylized arcade West, Kenney low-poly CC0 pipeline.
2. **Win condition** -> Bounty Run score race: 15 min, highest bounty wins, respawns on.
3. **Lobby size** -> 6–16, default 8, validated at 6/12/16.
4. **Loadout** -> starting rifle pick at spawn/respawn; no ground scavenging.
5. **Horse** -> per-match archetype pick; cosmetic-only persistence later; stats never persist.
6. **Audience** -> friends-first party game through alpha; public/ranked deferred.
7. **Production secret custody** -> intent: setec-backed vault with startup reconciliation
   (see D2 in `docs/decisions.md`); the in-memory fail-closed vault stands for the prototype.

Still open: the D5 ranked-results mechanism (intended: peer co-sign quorum) and the
production custody implementation details.
