# SPURFIRE — Prototype Plan (M0–M6)

**Status:** v2, reconciled 2026-07-20 against shipped code. Companion to `docs/design.md`
(pillars source of truth). Every number here is a *starting value for playtest iteration*,
not a commitment. Numbers over prose; tune with data.

**Prototype non-goals:** monetization, ranked/witness validation, accounts/persistent user
state, persistent named horses (per-match archetype pick until M4 data says otherwise),
singleplayer bots, slide/mantle/tac-sprint movement extensions.

## 0. Global constants

| Constant | Value | Notes |
|---|---|---|
| Rider HP | 100 | regen 8/s after 6s without damage |
| Horse HP | 250 (2.5x rider) | regen 10/s after 6s without damage |
| Gravity | 22.0 m/s^2 | shipped Godot default; arcade feel accepted. Dive numbers derive from this (M2) |
| Saddle height | 1.6 m | sets dive airtime (M2) |
| Engagement range r_e | 60 m | encounter math + "long hit" threshold |
| Match length target | 15 min (band 12–18) | Bounty Run |
| Sim / input / authority tick | 60 Hz | snapshots 20 Hz delta + 2 Hz MatchState keyframes, interpolation delay 100 ms (M6) |

## 1. Milestone ladder

| MS | Name | Status | Exit gate (one line) |
|---|---|---|---|
| M0 | Graybox horse locomotion | **done** | 3 archetypes rideable on 1.5km terrain at 60fps |
| M1 | Mounted shooting + sway | **done (as-built: SF rifles + ADS)** | sway model drives hit% into target bands |
| M2 | Saddle Dive + invited-friends path | **source complete / credentialed playtest pending** | two riders preserve movement/combat through epoch-2 failover; testers dive 2–4x/match |
| M3 | Spook/bolt, on-foot kit, Majestic Return | **source complete / playtest pending** | median lose-horse-to-remount < 40s |
| M4 | Spur meter + Majestic Charge | **source complete / playtest pending** | median player earns >=1 charge/match |
| M5 | Bounty Run scoring loop | **source complete / playtest pending** | 15-min match, winner 400–800 pts, "play again" >= 70% |
| M6 | Scale and qualify the complete loop | **implementation advanced / live qualification pending** | 8p peer-hosted match, migration < 3s with score intact |

Build order is strict: each milestone's tuning depends on the previous one's feel. The invited-
friends M2 movement/combat/failover source path is implemented now; it is not deferred to M6.
M6 retains scale, scoring/objective continuity, soak, joining churn, live qualification, and
release evidence after the M5 fun verdict.

Every gameplay milestone follows the established pattern: deterministic Rust kernel in
`spurfire-protocol`/`spurfire-gdext`, thin Godot adapter, headless smoke test.

---

## M0 — Graybox horse locomotion

**Goal:** riding feels good with placeholder art. If galloping isn't fun alone, nothing above matters.

**Mechanics in scope:** gait state machine (stand/walk/trot/gallop), accel/decel curves,
rein turn (rate-limited at speed), drift/power-turn (handbrake), jump, terrain speed factors,
slope momentum (+15% downhill accel, -15% uphill), camera (chase, speed FOV 70->78).

**Tuning table:**

| Parameter | Courser | Warhorse | Mustang |
|---|---|---|---|
| Walk m/s | 2.0 | 1.8 | 1.9 |
| Trot m/s | 5.0 | 4.2 | 4.6 |
| Gallop m/s | 14.5 | 12.0 | 13.0 |
| Sprint (Charge only) m/s | 16.5 | 13.5 | 14.5 |
| 0->gallop time s | 3.0 | 5.0 | 3.5 |
| Turn rate @ gallop deg/s | 60 | 45 | 80 |
| Turn rate @ walk deg/s | 140 | 120 | 150 |
| Drift turn deg/s (2s, -30% speed) | 120 | 90 | 150 |
| Jump apex m / airtime s | 1.8 / 0.7 | 1.2 / 0.5 | 2.2 / 0.8 |
| HP (placeholder, M3 owns) | 200 | 320 | 250 |

Terrain speed factors: grassland 1.0, riverbed 0.95, scrub 0.9, mud/marsh 0.7,
slope >20deg uphill 0.85. Mustang ignores scrub/riverbed penalty (0.95 floor).

**Acceptance checklist (playtest):**
- [ ] Tester can hold gallop across 500m of grassland without fighting the camera.
- [ ] Power-turn is usable to reverse direction within a 15m circle at gallop.
- [ ] Archetype difference is felt blind: testers identify Courser vs Warhorse in < 60s.
- [ ] Jump clears a 1.5m fence line reliably (>90% success at full gallop).
- [ ] 16 rider+horses on screen at 60fps on mid-tier target hardware.
- [ ] 1.5km-radius terrain streams without hitches > 16ms.

**Top risks:** (1) horse controller feel (too boaty / too twitchy) — budget 2 full iteration
passes; (2) camera nausea at gallop + drift; (3) large-world streaming stutter.

---

## M1 — Mounted shooting + sway model

**Status: done, as-built.** Shipped as the SF-series rifle trio (Dustwalker / Longspur /
Rattler) with ADS instead of revolver/repeater with lean-shoot; stat rows are locked in
`spurfire-protocol` and `docs/combat-m1.md` describes the result. Lean-shoot is dropped
(ADS + the sway stack carries the skill expression). The tables below are retained as
historical tuning intent; the protocol rows are authoritative.

**Goal:** aiming from horseback is skill-expressive: stable on straights, punished in turns.

**Mechanics in scope (as planned):** two weapons (revolver, repeater), fire while any gait,
lean-shoot +/-90deg, aim cone driven by sway multiplier stack, hit feedback.

**Sway model (multiplies weapon base cone):**

`sway = gait_base x turn_factor x terrain_factor + impulses`

| Term | Values |
|---|---|
| Gait base | stand 1.0 / walk 1.1 / trot 1.35 / gallop 1.6 / sprint 1.8 |
| Turn factor | 1 + (turn_rate_deg_s / 90), cap 2.0 |
| Terrain factor | grass 1.0 / riverbed 1.15 / scrub 1.25 / mud 1.4 |
| Jump landing impulse | +0.5, linear decay over 1.2s |
| Hit taken impulse | +0.4, decay 0.8s |
| Collision impulse | +0.8, decay 1.5s |

**Weapons (M1 placeholders):**

| Weapon | Dmg | ROF | Base cone deg | Falloff | Mag / reload |
|---|---|---|---|---|---|
| Revolver | 34 | 1.2/s | 0.8 | 40–80m to 50% | 6 / 2.2s |
| Repeater | 22 | 3.0/s | 1.0 | 60–120m to 60% | 8 / 2.8s |

**Acceptance checklist:**
- [ ] Stationary target at 40m: >=80% hits straight-line gallop on grass, <30% in full drift turn.
- [ ] Testers route around mud/scrub when dueling (terrain reads as accuracy choice).
- [ ] TTK mounted-vs-mounted: 2.5–4.0s revolver at 40m under gallop sway.
- [ ] Lean-shoot usable without camera fight; 180deg switch < 0.4s.
- [ ] Sway HUD communicates state without reading numbers (5-point icon scale).

**Top risks:** (1) sway frustrating rather than expressive — tune impulse decay first, gait
base last; (2) repeater dominates revolver at all ranges; (3) aim assist needs for gamepad
underestimated.

---

## M2 — Saddle Dive

**Status: implementation complete / playtest pending.** Deterministic Rust, GDExtension,
wire 1.1 stance, thin Godot presentation, and forced bounded smoke are implemented. Those
gates prove mechanics, not the natural frequency, effectiveness, death-rate, notification,
or feel targets below; M2 is not done until observational playtest evidence passes them.

**Goal:** signature mechanic works and is *measurable*: high risk, high reward, never dominant.

**Mechanics in scope:** dive via the dismount input at speed (>=8 m/s — no dedicated
button), launch vector, airborne accuracy window, horse continues, landing recovery, and
deterministic notifications (FLYING DISMOUNT, SADDLE DIVE HEADSHOT, FULL-GALLOP HIT,
AIRBORNE REVERSAL). These events award no points; M5 owns scoring. Airborne fire is
**dive-only**; the kernel keeps rejecting normal jump-air fire. Groundwork: snapshot DTOs
carry a stance field from M2 onward so D7 lag-comp rewind needs no later wire break.

**Tuning table** (airtime numbers re-derived for shipped gravity 22.0; the v1 values
assumed 9.8 and would have produced ~0.57s of air):

| Parameter | Value | Derivation |
|---|---|---|
| Min speed to dive | 8 m/s | trot excluded for Courser (5.0), gallop-only |
| Launch impulse (chosen dir) | 6.0 m/s, cone +/-75deg from velocity | covers "reversal" launches |
| Vertical pop | 6.0 m/s | with saddle 1.6m at g=22: airtime ~= 0.74s; target band 0.7–0.9s |
| Airborne sway | x0.6 (40% steadier) | bonus decays in final 20% of airtime |
| Airborne shots | Longspur 1 / Dustwalker 3 / Rattler 5 | ~= cadence x airtime; hard cap, no reload airborne |
| Horse behavior | continues 25m, decel 2s, idles | retrievable but not instant |
| Landing recovery | 0.4s prone no-input + 0.4s 50% move, no fire | 0.8s total vulnerability |
| Bad landing (slope >30deg) | +15 dmg, +0.4s recovery | terrain punishes blind dives |
| Dive cooldown | none — cost is dismount + recovery | remount is the gate |

**Acceptance checklist:**
- [ ] Airborne hit% is measurably above mounted-gallop baseline (target +25–40% relative).
- [ ] Deaths within 3s of landing: 25–40% of dives (risk is real, not suicidal).
- [ ] Testers use dive 2–4x per 15-min match (not 0, not 10).
- [ ] All four notifications trigger naturally within 3 matches.
- [ ] AIRBORNE REVERSAL presentation lands without animation popping. The legacy
  “behind launch” wording is blocked: a +/-75-degree launch cone cannot produce a >90-degree
  launch. M2 logs reversal only when an authority-confirmed dive shot points strictly behind
  pre-launch velocity; the cone remains locked pending product-documentation correction.

**Top risks:** (1) dive becomes optimal spam (recovery too weak) or never used (recovery too
strong) — this is the single most sensitive number pair in the prototype (airborne sway x0.6 /
recovery 0.8s); (2) animation blending quality sells the move — graybox dive that *looks*
broken will poison playtest feedback; (3) airborne FOV/camera motion sickness.

---

## M3 — Horse vitality, spook/bolt, on-foot kit, Majestic Return

**Status: source complete / playtest pending.**
`spurfire-protocol::m3` now owns replay-safe archetype vitality and regeneration, the fatal
spook/three-second bolt edge, exact on-foot stamina/crouch/roll/stun timing, recall reductions,
the Majestic Return phase clock, running-mount validation, acceptance telemetry, and validated
migration checkpoints. A bounded native actor bank now routes unique horse target IDs, enforces
authority epochs, and migrates the canonical roster atomically; `M3GameplayController` exposes that
owner to Godot with a headless spook-to-running-remount regression. `M3CombatAuthority` adds a
rewindable horizontal horse-body capsule/head sphere and commits ammo, target health, horse
vitality, and fatal-spook effects transactionally without misreporting a horse bolt as a rider
elimination. `M3MatchCheckpointV2` binds that gameplay bank to the existing sorted combat roster,
ammo, health, command receipts, authority epoch, and canonical digest. M3 changes signed actor
input/snapshot/checkpoint canonicalization, so strict wire 2.0 defines
bounded loadouts, mounted/on-foot input, complete actor/horse snapshots, hash-bound MTU-safe
migration fragments, and fixed-layout signing bytes.
`M3SecureSession` now applies exact-roster signatures, endpoint identity, replay/role checks, and
atomic out-of-order checkpoint assembly; composed combat restoration validates the complete
rider/horse target graph before installation. The live lobby advertises wire 2.0, derives one
immutable loadout graph from the control-authoritative roster horse choices and locked Alpha rifle,
and activates it before secure manifest binding. `PeerSession` keeps the RustScale worker
wire-opaque, while `SpurfireLobbyPeerBridge` advances and rewinds rider/horse state through
`M3CombatAuthority`, emits complete actor snapshots, and uses fragmented atomic migration. Legacy
wire 1.2 remains only as the M2 proof/demo codec. The composed authority now owns a separate
rollback-safe reload clock: an admitted reload pauses across stun/roll, resumes without losing
progress, completes against canonical ammo, and migrates in an exact player-sorted wire-v2 row.
The live scene boundary latches jump/crouch for exactly nine 60 Hz ticks, and the native on-foot
kernel checkpoints explicit walk/sprint/crouch acceleration and deceleration. Authority snapshots
drive a separate interpolated remote-horse proxy through spook, 12 m/s bolt, dust reveal, exact
three-second Majestic Return, mount window, and running mount presentation. The authority also
writes secret-free per-actor-slot interval and event rows for stance time, roll/stun time, horse
losses, remount duration, running-mount attempts/success, cross-stance eliminations, post-spook
deaths, and the fixed 15-point horse-loss notification. `scripts/aggregate-playtest.py` validates
and summarizes those rows. Automated checks establish the source path, not the observational exit
gate below; M3 remains playtest pending until real sessions meet it.

**Goal:** losing your horse is a dramatic mid-match arc, not a death sentence — and on-foot
play is a real, butter-smooth kit that stays deliberately weaker than mounted play.

**Mechanics in scope:** horse HP/damage, spook at 0 HP (throw rider, bolt), on-foot kit
(sprint / crouch / roll, its own deterministic stance kernel mirroring `HorseKernel`),
designed headglitch cover standard, whistle recall (trimmed economy), Majestic Return
sequence, running mount.

**Tuning table:**

| Parameter | Value |
|---|---|
| Horse HP | Courser 200 / Mustang 250 / Warhorse 320 (2.0–3.2x rider) |
| Spook throw | rider lands 3m lateral, 0.6s stun, no fall damage |
| Bolt | horse sprints away from last damage source 3s, then despawns |
| On-foot move | walk 2.0 / sprint 4.5 m/s (4s stamina, 6s regen) |
| On-foot sway | stand 0.9 / move 1.2 / sprint 1.5 / crouch 0.8 (steadiest stance) |
| Crouch | hold Ctrl: 1.2 m/s, lowered capsule + eye height |
| Headglitch cover standard | course low cover authored at 1.0–1.1m; crouched eye-line clears it, body protected; head remains headshot zone |
| Tactical Roll | tap Ctrl while sprinting: 0.5s, ~3.5m displacement, no fire during, +0.3 sway on exit (0.6s decay), 1.5s cooldown |
| Roll hitbox | crouch-height capsule for the duration; **no i-frames** (peer-authority fairness) |
| Roll x reload | roll cancels reload; progress resumes after |
| Input feel | 0.15s input buffer on roll/jump/crouch; explicit accel/decel curves per stance; cancel windows in this table, never animation-length |
| Base recall timer | 20s |
| Recall reduction: damage dealt | -1s per 25 dmg |
| Recall reduction: objective tick | -2s per tick |
| Recall floor | 8s |
| Majestic Return sequence | whistle -> 2s hoofbeats -> 1.5s dust/silhouette -> gallop-in 3s -> slide stop |
| Running mount window | 1.5s, within 4m of moving horse, mounts at trot speed |

Cut from v1: feed pickups, hitching posts, and survive-under-fire recall reductions — three
map/UX systems for marginal tuning value and zero mid-fight legibility.

**Acceptance checklist:**
- [ ] Median lose-horse-to-remount wall-clock < 40s (target 30–35s).
- [ ] On-foot player wins 25–35% of duels vs mounted (viable, not preferred).
- [ ] Players spend >= 70% of match time mounted (identity guardrail; if crouch-camping
      appears, dial recall faster before touching the kit).
- [ ] One-clause test: new tester rolls and headglitches within 2 minutes of hearing
      "tap crouch while sprinting" / "crouch behind low cover".
- [ ] Bolt feels earned: shooter sees the 15-pt notification (M5 wires score; M3 logs event).
- [ ] Return sequence reads at distance: testers stop fighting to watch >= first time.
- [ ] Running mount success >= 70% on first approach after 3 matches of practice.

**Top risks:** (1) on-foot kit too strong inverts the mounted identity — watch the 70%
mounted-time guardrail; (2) on-foot loop so weak that spook = free kill for attacker (watch
post-spook death rate, target < 50%); (3) return pathing (horse navigating terrain to reach
rider) is an AI rabbit hole — cheat with off-camera spawn at 60m if needed for the
prototype.

---

## M4 — Spur meter + Majestic Charge

**Status: source complete / playtest pending.** The checkpointed native meter validates the locked
award rows, enforces no decay and an 18-point movement-style lifetime cap, and owns rising-edge
mounted Charge versus horseless instant-Return spending. Live wire 2.0 consumes the reserved Q bit
and snapshots meter/charge timing. Authority-observed movement, hostile near-miss, and transactional
combat producers issue the only credits. Charge applies archetype sprint speed, x2 acceleration,
+30% turn, zero drift cost, terrain factor 1.0, x0.7 weapon sway, and the body/headshot stagger
distinction. Godot supplies the meter/readiness/countdown HUD, 50/80/100 audio tiers, local/remote
presentation, and secret-free source, spend, exposure, per-actor frequency, fill-time, and duel
telemetry. Automated coverage establishes the implementation contract; the human checklist below
still requires invited-player evidence.

**Goal:** reward stylish riding with a readable power spike that doesn't break balance.
One meter, one button (Q), effect depends on state.

**Mechanics in scope:** Spur meter 0–100 with exactly four fill sources and **no decay**;
one spend: mounted -> Majestic Charge; horseless -> instant Majestic Return (consumes
meter, no charge on arrival).

Cut from v1 (was "Bond meter"): sustained-gallop trickle, return-to-horse fill, and idle
decay — the economy must be countable on one hand and readable mid-fight.

**Tuning table:**

| Spur source | Points |
|---|---|
| Jump +4 / clean landing (no collision, <0.2 sway impulse) +2 | movement style |
| Near miss (projectile within 1.5m) | +3 |
| Mounted hit +2 / mounted elim +6 | mounted combat |
| Saddle Dive elim | +8 (largest: dive is the crown jewel) |

| Majestic Charge | Value |
|---|---|
| Duration | 6s (design band 5–7s) |
| Accel | x2, top speed = archetype sprint |
| Turn rate | +30%, drift cost removed |
| Sway | x0.7, terrain factor forced to 1.0 |
| Stagger | immune except headshots |
| Horse absent + full meter | Q = immediate Majestic Return (consumes meter) |

Expected fill time: ~4–6 min of active stylish play -> 1–2 charges per 15-min match.

**Acceptance checklist:**
- [ ] Median player earns >= 1 charge/match; top quartile <= 3.
- [ ] Charge win-rate delta in duels: +15–25% (strong, not autowin).
- [ ] Spur economy can't be farmed solo (jump loops alone fill < 20% of a match's meter).
- [ ] Instant-return branch triggers correctly and feels heroic, not like skipping punishment.
- [ ] Meter legibility: testers can state their charge readiness without looking (audio tiers).

**Top risks:** (1) charge as escape button devalues dives (usage overlap) — monitor dive rate
pre/post M4; (2) Spur farming routes (jump lines) distort movement; (3) full-meter instant
return may be stronger than charge — A/B which players pick; (4) no decay means meters may
sit full — if hoarding dominates, add a gentle spend incentive before any decay math.

---

## M5 — Bounty Run scoring loop

**Status: source complete / playtest pending.** The pure Rust match kernel owns the exact 15-minute
clock, canonical scoreboard and category breakdown, strict five-second assist window, respawn and
speed-buff timers, deterministic Most Wanted reveals/survival payouts, dynamic-objective cadence,
locked objective payouts, long-hit cap, winner tie-break, and fail-closed epoch checkpoint. Combat
score producers update a cloned combat/match transaction, and the signed wire-2.0 checkpoint now
restores both authorities only after exact epoch, tick, and roster validation. Authority-only MatchState
keyframes are signed at 2 Hz, bind their epoch/tick/roster, stay within the 1,200-byte datagram cap
for eight worst-case timer rows, and drive the follower match clock, local bounty, pressure banner,
K/A/D scoreboard, respawn/buff status, and route health. The Godot world resizes to the locked
roster-scaled radius; integer placement derives edge-buffered objectives and separated outer-ring
respawns from the lobby seed. Full rider/horse/combat respawns, the ten-second speed buff, all five
objective interactions, the moving-bounty marker, and Most Wanted flare are wired. Ammo-wagon
refill is atomic with its score award, and the horse station grants the locked 60-second +10% speed
buff. A centered results panel ranks the final scoreboard, explains each score category, and records
the play-again choice. Final-only category rows remain within the live datagram cap. The elected
authority submits the bounded final ledger through the native participant-capability boundary and
then follows the existing safe teardown path. Secret-free one-second M5 intervals and the deterministic
playtest aggregator measure winner pacing, category diversity, objective share, Most Wanted pressure,
dead/encounter/objective time, worst convergence gap, and play-again rate. Automated coverage proves
the implementation contract; every acceptance checkbox below still needs human match evidence.

**Goal:** full 15-min matches with score pressure, Most Wanted drama, and event-driven
convergence. Fun verdict gate for the whole prototype.

**Mechanics in scope:** scoring feed, respawns, Most Wanted reveal, dynamic objectives,
end-of-match results.

**Tuning table:**

| Score event | Points |
|---|---|
| Elimination | 100 |
| Assist (>=30 dmg, <5s) | 50 |
| Force horse to bolt | 15 |
| Saddle Dive elim (bonus) | +25 |
| Mounted long hit (>60m) | +10 per hit, cap +50/match |
| Dynamic objective | 50–150 by type |
| Most Wanted elim (bonus) | +75 |
| Most Wanted survival tick | +10 per reveal survived |

| Match rule | Value |
|---|---|
| Duration | 15 min |
| Respawn | 5s, outer 70–85% of radius, min-distance placement, +20% move speed for 10s (kills the ride-back slog) |
| Most Wanted reveal | leader, every 60s, 10s duration (map ping + flare + birds) |
| Objective cadence | 1 event per 90s, 60s lifetime, >=150m from map edge |
| Objective payouts | moving bounty 150 / supply herd 100 / ammo wagon 80 + full equipped-ammo refill / signal tower 50 per 10s held (max 150) / horse-buff station 50 + 60s, +10% speed buff |
| Score pacing target | winner 400–800, median 200–350 |

**Acceptance checklist:**
- [ ] Match length lands in 12–18 min without time-limit blowouts (score cap not needed).
- [ ] Winner uses >= 2 score categories beyond raw elims (objectives matter).
- [ ] Most Wanted holder's bounty gain slows measurably during reveals (pressure works).
- [ ] No >90s stretch without an encounter or objective for any player (dead-time audit).
- [ ] Post-match survey: >= 70% "would play again"; dive/bolt/return each named as a
      highlight by >= 30% of testers.

**Top risks:** (1) snowballing — Most Wanted bonus may feed the leader; survival tick is the
counterweight, watch Gini coefficient of scores; (2) 6-player lobbies feel empty (see
encounter math below); (3) respawn ride-back time at 16p radius (~50–75s to center) — the
+20% respawn speed buff is now a default; if still punishing, dial objective cadence 90s ->
60s next.

---

## M6 — Networked lobby: complete the loop

**Goal:** everything above works over a real tailnet with one elected authority, 8+ players.

**Status: the complete gameplay state and peer-owned failover source paths are built; scale and
live qualification remain** — the lobby locks and projects the exact match-start election input,
survivors recompute the same rule after two seconds of authority silence, and the service validates
the next-epoch claim without selecting it. Signed wire 2.0 carries M3–M5 authority state and a
bounded hash-checked checkpoint; the new authority restores combat, Spur, match score/clock,
objectives, receipts, and RNG state atomically. Actor snapshots broadcast as field-level deltas at
20 Hz against recoverable 2 Hz full bases, while MatchState
keyframes at 2 Hz, presentation retains 600 snapshots (at least ten seconds), and shot admission
uses the locked 150 ms rewind cap over a 250 ms pose/stance history. Native zeroizing create/join
handoff and the privacy-suppressed public stats endpoint are implemented. Credential-free tests
and a three-process authority-kill proof verify exact fragmented checkpoint installation and M5
score, clock, and objective continuity. 8–16-peer/soak harnesses, credentialed packaged-client
runs, and human evidence are still outstanding.

**M6 source scope and current state:**

1. **One migration rule, peers own it.** Mid-match authority is decided by peers: on 2s
   authority silence, every survivor recomputes `election_v1` over the match-start
   measurement matrix restricted to the survivor set — deterministic, coordination-free.
   `SessionState.expire_and_migrate`'s lowest-ID rule becomes the degraded fallback *inside*
   the same protocol scoring function; the server's scored re-election applies only in
   `READY`, and during `IN_MATCH` the service validates the successor's heartbeat by
   recomputing the same function. Split-brain prevented by construction + existing epochs.
2. **Complete-state handoff.** The signed checkpoint carries per-rider movement/health/ammo,
   input/shot receipts, M5 score and clock, Spur, objectives, and RNG counter and verifies the
   restored-state hash before epoch continuation. MatchState keyframes remain MTU-safe at eight
   players and the presentation ring retains at least ten seconds. The 20 Hz actor stream sends
   signed field-level deltas against 2 Hz full bases; a peer missing its base fails closed until the
   next full packet. The three-process credential-free proof kills authority A, installs B's
   fragmented checkpoint on C, then verifies exact score, clock, and objective continuity.
3. **Lag compensation: authority-side rewind, capped 150ms.** `CombatAuthority` keeps a
   ~250ms position+stance history; `ShotCommand` carries the shooter's view tick; rewind is
   capped at 150ms (beyond that you lead). Stance-aware hitboxes (crouch/roll) rewind too.
4. **Client join flow.** The gated Alpha shell drives one-use create/invitation/join,
   capability-bound key proof, server-signed exact-roster endpoint/session-key projection,
   wire 2.0 signed native source-checked traffic, route/RTT election reports, creator start,
   peer Leave, self-leave, and truthful teardown into `PeerSession`. Create/join HTTPS and
   first-response secret handling are native and zeroizing; GDScript receives only redacted public
   JSON. Real activation remains dark pending credentialed packaged-client and cleanup evidence.
   Per-lobby join code; no accounts.
5. **Landing-page live stats.** Secret-free aggregate stats endpoint feeding
   spurfire.rajsingh.info: riders online, lobbies by state, direct-connection rate, median
   RTT. No lobby IDs or join material; all real metrics are suppressed below a cohort of three,
   which means the singleton Alpha always publishes only the suppression state.

**Tuning table:**

| Parameter | Value |
|---|---|
| Sim / authority / input tick | 60 Hz |
| Snapshot broadcast | 20 Hz (delta-compressed) + 2 Hz MatchState keyframes |
| Remote interpolation delay | 100ms (2 snapshot intervals) |
| Prediction window | horse state predicted; shots authority-validated |
| Snapshot retention | ring buffer 10s on every peer (migration source) |
| Lag-compensation rewind cap | 150ms (position + stance history ~250ms) |
| Authority failover | detect 2s silence -> deterministic re-election -> restore + announce < 3s total |
| Election scoring | weights: direct-conn count 0.3, median RTT 0.25, worst RTT 0.15, jitter 0.1, loss 0.1, upload 0.1 |
| Match sizes validated | 6, 12, 16 |

**Acceptance checklist:**
- [ ] 8-player match completes end-to-end on a real tailnet (create -> play -> results -> teardown) driven from the game client, not scripts.
- [ ] Median RTT < 80ms direct; lobby health UI matches measured matrix.
- [ ] Kill the authority mid-match: play resumes < 3s, score/state intact (<= 1 keyframe interval of loss); extend `p2p-live` to assert score continuity.
- [ ] Authority-vs-peer hit% gap < 5% (bot-duel fairness harness).
- [ ] No movement desync > 200ms peak for any client over a 15-min soak.
- [ ] Forced-DERP + packet-loss soak playable: TTK consistency holds, sway model unaffected.
- [ ] 16-peer churn run (joins/leaves mid-lobby) without stuck state.
- [ ] Ephemeral devices and tailnet cleaned up after match (no leaked state).

**Top risks:** (1) RustScale is alpha + sibling repo bugs — budget integration slack, log
everything, keep the one-tailnet-with-tags fallback warm; (2) rewind interacting with the
sub-second dive window — validate dive duels under 100ms+ artificial latency early;
(3) NAT traversal failure rate in the wild (friends-and-family tailnet first).

---

## 2. Encounter-frequency math

Radius: `R = clamp(450, 250*sqrt(n), 1500)`. Model: mean encounter time
`t = A / ((n-1) * 2 * r_e * v)` with A = pi*R^2, r_e = 60m engagement range,
v = 13 m/s average gallop (blend of archetypes).

| n | R (m) | A (km^2) | Cross-map gallop (2R/v) | t_random (s) | t with event pull (x0.4) |
|---|---|---|---|---|---|
| 6 | 612 | 1.18 | 94s | 151 | 60 |
| 12 | 866 | 2.36 | 133s | 137 | 55 |
| 16 | 1000 | 3.14 | 154s | 134 | 54 |

**Key insight:** because R scales with sqrt(n), density is ~constant (1 player per 0.196 km^2),
so random-roam encounter time is flat at ~135–150s regardless of lobby size. The formula
guarantees consistent *feel*; it does not by itself deliver action.

**Design levers, in pull order:**
1. **Event convergence (x0.4 assumed above):** 90s objective cadence pulls 40–60% of the lobby
   into a ~300m zone; local density rises 8–10x -> TTE inside events ~10–15s. The x0.4 factor
   is the least-certain number in this doc; validate in M5 with dead-time audit.
2. **Most Wanted reveal** forces chases independent of events (~every 60s).
3. **Respawn geometry** (outer 70–85%) routes riders inward past each other.
4. If median TTE still > 75s at 6 players: raise event cadence 90s -> 60s *before* shrinking
   the radius floor (450m), which would break the open-range fantasy.
5. Gallop speeds (12.0–14.5 m/s) are set so cross-map is 94–154s: long enough for territory to
   matter, short enough that a 5s respawn + ride-back is ~1/6 of match time at worst. Do not
   raise speeds to fix TTE — fix convergence instead.

---

## 3. Engine-decision annex (decided: Godot 4 + Rust GDExtension — see D4; retained as historical rationale)

| Engine | Horse/anim tooling | 1.5km world streaming | RustScale (Rust crate) embedding | Iteration speed | Prototype risk |
|---|---|---|---|---|---|
| **Bevy** | Immature anim graph; build our own blends | DIY; ECS handles 16 horses trivially | **Native: cargo dependency, zero FFI** | Fast compile-run for sim logic; slow for content | Tooling gaps cost time at M2 (anim) |
| **Godot 4** | AnimationTree adequate for dive/landing blends | Built-in streaming OK at 1.5km | **gdext GDExtension: Rust in-process, idiomatic** | Fastest content iteration | Large-world perf unproven at 16 riders |
| **Unity** | Mature (Animator, Animancer asset) | Proven (addressables, terrain) | C FFI via cbindgen + P/Invoke, or sidecar over localhost UDP | Fast content, medium build | FFI boundary bugs; license cost; Netcode unused (custom authority anyway) |
| **Unreal 5** | Best-in-class (Motion Matching) — dive would look AAA | Best (World Partition) | C staticlib FFI; build-system friction | Slowest iteration; heaviest team ramp | Overkill; iteration speed kills tuning loops |

**Common truth:** the networking model (custom peer-hosted authority) is engine-agnostic; no
engine's built-in netcode fits, so RustScale integration is the differentiator.

**Evidence that decides (run as 2-week spikes, top-2 candidates, during M0):**
1. Days to M0 exit gate (rideable graybox, 60fps, 1.5km terrain).
2. Days to first RustScale packet in-process; crash-free soak hours over 1 week.
3. Dive animation blend quality achievable by a non-specialist (M2 is the visual make-or-break).
4. 16 horses + riders + projectiles at 60fps on mid-tier hardware.
5. Streaming 1.5km radius at gallop (14.5 m/s) without >16ms hitches.
6. Hot-reload / iteration time on a sway-constant tweak (tuning-loop velocity).

Decision gate (historical): end of M0 spike. Godot won — the M0/M1 slices shipped on
Godot 4.7.1 with gdext and the acceptance checks held. See `docs/decisions.md` D4.

---

## 4. Playtest instrumentation

### M2 Saddle Dive risk/reward

The secret-free schema-v1 row is keyed by `(actor, dive_id)` and records authority epoch;
launch tick, locked weapon/gait, pre-launch velocity and speed; requested/clamped direction
and angle; clamp flag; horizontal impulse, vertical pop, launch height, resulting planar and
total speed, and nominal airtime; landing tick and actual airtime; shot attempts, accepted
shots, hits, headshots, reversal hits, and damage dealt; landing terrain, quantized slope,
outcome, and landing damage; damage taken and death within the inclusive landing-through-3s
window; remount tick/time; and terminal censor reason. The client appends exactly one
allowlisted finalized/censored row to a per-session JSONL file under `user://logs`; each line
is flushed so an interrupted session remains parseable and prior sessions are preserved.
Session rows include a random local session ID, schema, build identifier, and fixed simulation
rate. They contain no score delta, bond gain, credential, capability, join code, seed,
endpoint, or client-claimed style credit.

| Metric | Target band | Dial if out of band |
|---|---|---|
| Dives per player per 15-min match | 2–4 | recovery 0.8s +/- 0.2s |
| Airborne hit% vs gallop baseline | +25–40% relative | airborne sway x0.6 (range 0.5–0.8) |
| Deaths within 3s of landing | 25–40% | recovery length; bad-landing penalty |
| Natural notification coverage | all four within 3 matches | presentation clarity, then shot cap |
| Median time-to-remount after dive | 6–10s | horse continue distance (25m) |
| AIRBORNE REVERSAL hit share | observe first; no launch-angle target while contradiction is blocked | shot-direction skill window; do not widen launch cone |
| Sickness reports (camera) | ~0 | keep no kick/dip/roll first; tune only from evidence |

**Deferred metrics:** M4 owns bond gain and its value; M5 owns outcome score delta, dive-elim
share, score bonuses, and match-half reward pressure. M2 instrumentation must not synthesize
those future systems.

### M3 horse-loss and on-foot arc

The authority appends one-second `m3_interval` counters plus discrete horse-loss, remount, and
rider-elimination rows to a separate per-session JSONL file. Stable roster UUIDs are converted to
sorted integer actor slots before persistence; credentials, endpoints, join codes, and player IDs
are never stored. Terminal and authority-loss flushes preserve partial observations. The shared
aggregator reports mounted/on-foot share, roll and stun seconds, horse losses, remount count and
median duration, rising-edge running-mount success, cross-stance win rate, post-spook death rate,
and exact 15-point bolt-notification coverage.

### M4 Spur economy and Charge value

The same secret-free interval stream records authority-awarded points by locked source, Charge and
full-meter exposure ticks, contextual spends, and charged/uncharged duel outcomes. Discrete spend
rows preserve the first-Charge tick without player identifiers. The aggregator groups sorted actor
slots per session to report median and p75 Charge counts, median minutes to first Charge, Charges per
15 player-minutes, movement-style share, full-meter hoarding, instant-Return choice share, and Charge
win-rate delta. These are observational evidence only: M5 owns score and match outcomes.

Kill criteria (redesign, not tune): dive elim share >25% (dominant) for 2 consecutive
milestones, or usage <1/match after two buff passes (players have voted it's not fun).

---

## Appendix: tuning-dial priority

When playtest data conflicts, turn dials in this order before touching anything structural:
(1) impulse decay times, (2) score bonuses, (3) recall reductions, (4) recovery windows,
(5) sway bases, (6) speeds, (7) radius formula constants. Lower numbers = cheaper iteration.
