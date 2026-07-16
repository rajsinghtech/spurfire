# SPURFIRE — Prototype Plan (M0–M6)

**Status:** v1. Companion to `docs/design.md` (pillars source of truth). Every number here is a
*starting value for playtest iteration*, not a commitment. Numbers over prose; tune with data.

**Prototype non-goals:** visual setting, monetization, ranked/witness validation, persistent
named horses (per-match archetype pick until M4 data says otherwise), singleplayer bots.

## 0. Global constants

| Constant | Value | Notes |
|---|---|---|
| Rider HP | 100 | regen 8/s after 6s without damage |
| Horse HP | 250 (2.5x rider) | regen 10/s after 6s without damage |
| Gravity | 9.8 m/s^2 | no arcade fudge until M2 dive data demands it |
| Saddle height | 1.6 m | sets dive airtime (M2) |
| Engagement range r_e | 60 m | encounter math + "long hit" threshold |
| Match length target | 15 min (band 12–18) | Bounty Run |
| Authority tick | 30 Hz | snapshots 20 Hz, client input 60 Hz (M6) |

## 1. Milestone ladder

| MS | Name | Exit gate (one line) |
|---|---|---|
| M0 | Graybox horse locomotion | 3 archetypes rideable on 1.5km terrain at 60fps |
| M1 | Mounted shooting + sway | sway model drives hit% into target bands |
| M2 | Saddle Dive | measurable risk/reward; testers dive 2–4x/match |
| M3 | Spook/bolt, on-foot loop, Majestic Return | median lose-horse-to-remount < 40s |
| M4 | Bond meter + Majestic Charge | median player earns >=1 charge/match |
| M5 | Bounty Run scoring loop | 15-min match, winner 400–800 pts, "play again" >= 70% |
| M6 | Networked lobby via spurfire-ctl + RustScale | 8p peer-hosted match, authority migration < 3s |

Build order is strict: each milestone's tuning depends on the previous one's feel.

---

## M0 — Graybox horse locomotion

**Goal:** riding feels good with placeholder art. If galloping isn't fun alone, nothing above matters.

**Mechanics in scope:** gait state machine (stand/walk/trot/gallop), accel/decel curves,
rein turn (rate-limited at speed), drift/power-turn (handbrake), jump, terrain speed factors,
slope momentum (+15% downhill accel, -15% uphill), camera (chase, speed FOV 70->85).

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

**Goal:** aiming from horseback is skill-expressive: stable on straights, punished in turns.

**Mechanics in scope:** two weapons (revolver, repeater), fire while any gait, lean-shoot
+/-90deg, aim cone driven by sway multiplier stack, hit feedback.

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

**Goal:** signature mechanic works and is *measurable*: high risk, high reward, never dominant.

**Mechanics in scope:** dive input (>=8 m/s, i.e. trot+), launch vector, airborne accuracy
window, horse continues, landing recovery, scoring notifications (FLYING DISMOUNT,
SADDLE DIVE HEADSHOT, FULL-GALLOP HIT, AIRBORNE REVERSAL).

**Tuning table:**

| Parameter | Value | Derivation |
|---|---|---|
| Min speed to dive | 8 m/s | trot excluded for Courser (5.0), gallop-only |
| Launch impulse (chosen dir) | 6.0 m/s, cone +/-75deg from velocity | covers "reversal" launches |
| Vertical pop | 3.5 m/s | with saddle 1.6m: airtime = 1.03s |
| Airborne sway | x0.6 (40% steadier) | bonus decays in final 20% of airtime |
| Airborne shots | revolver 2 / repeater 3 | hard cap, no reload airborne |
| Horse behavior | continues 25m, decel 2s, idles | retrievable but not instant |
| Landing recovery | 0.4s prone no-input + 0.4s 50% move, no fire | 0.8s total vulnerability |
| Bad landing (slope >30deg) | +15 dmg, +0.4s recovery | terrain punishes blind dives |
| Dive cooldown | none — cost is dismount + recovery | remount is the gate |

**Acceptance checklist:**
- [ ] Airborne hit% is measurably above mounted-gallop baseline (target +25–40% relative).
- [ ] Deaths within 3s of landing: 25–40% of dives (risk is real, not suicidal).
- [ ] Testers use dive 2–4x per 15-min match (not 0, not 10).
- [ ] All four scoring notifications trigger in normal play within 3 matches.
- [ ] Reversal dive (behind launch) lands without animation popping.

**Top risks:** (1) dive becomes optimal spam (recovery too weak) or never used (recovery too
strong) — this is the single most sensitive number pair in the prototype (airborne sway x0.6 /
recovery 0.8s); (2) animation blending quality sells the move — graybox dive that *looks*
broken will poison playtest feedback; (3) airborne FOV/camera motion sickness.

---

## M3 — Horse vitality, spook/bolt, on-foot loop, Majestic Return

**Goal:** losing your horse is a dramatic mid-match arc, not a death sentence.

**Mechanics in scope:** horse HP/damage, spook at 0 HP (throw rider, bolt), on-foot moveset,
whistle recall with reduction economy, Majestic Return sequence, running mount.

**Tuning table:**

| Parameter | Value |
|---|---|
| Horse HP | Courser 200 / Mustang 250 / Warhorse 320 (2.0–3.2x rider) |
| Spook throw | rider lands 3m lateral, 0.6s stun, no fall damage |
| Bolt | horse sprints away from last damage source 3s, then despawns |
| On-foot move | walk 2.0 / sprint 4.5 m/s (4s stamina, 6s regen) |
| On-foot sway | stand 0.9 / move 1.2 / sprint 1.5 (steadier than saddle, slower) |
| Base recall timer | 25s |
| Recall reduction: damage dealt | -1s per 25 dmg |
| Recall reduction: objective tick | -2s per tick |
| Recall reduction: feed pickup | -5s (map pickup, 60s respawn) |
| Recall reduction: hitching post | -3s (channel 2s at fixed posts) |
| Recall reduction: survive under fire | -1s per 5s alive while damaged, cap -3s |
| Recall reduction cap | 60% (min recall 10s) |
| Majestic Return sequence | whistle -> 2s hoofbeats -> 1.5s dust/silhouette -> gallop-in 3s -> slide stop |
| Running mount window | 1.5s, within 4m of moving horse, mounts at trot speed |

**Acceptance checklist:**
- [ ] Median lose-horse-to-remount wall-clock < 40s (target 30–35s).
- [ ] On-foot player wins >= 20% of duels vs mounted (viable, not preferred).
- [ ] Bolt feels earned: shooter sees the 15-pt notification (M5 wires score; M3 logs event).
- [ ] Return sequence reads at distance: testers stop fighting to watch >= first time.
- [ ] Running mount success >= 70% on first approach after 3 matches of practice.

**Top risks:** (1) 25s base recall may be brutally long in a 15-min match — first candidate
dial; (2) on-foot loop so weak that spook = free kill for attacker (watch post-spook death
rate, target < 50%); (3) return pathing (horse navigating terrain to reach rider) is an AI
rabbit hole — cheat with off-camera spawn at 60m if needed for the prototype.

---

## M4 — Bond meter + Majestic Charge

**Goal:** reward stylish riding with a readable power spike that doesn't break balance.

**Mechanics in scope:** bond meter 0–100 with fill economy and decay, Majestic Charge
activation, full-bond instant Majestic Return when horse absent.

**Tuning table:**

| Bond source | Points |
|---|---|
| Jump | +4 |
| Clean landing (no collision, <0.2 sway impulse) | +2 |
| Sustained gallop | +1 per 3s |
| Near miss (projectile within 1.5m) | +3 |
| Mounted hit | +2 |
| Mounted elim | +6 |
| Saddle Dive elim | +8 |
| Return to horse / running mount | +5 |
| Decay | -1 per 5s while idle or on foot (paused while horse absent) |

| Majestic Charge | Value |
|---|---|
| Duration | 6s (design band 5–7s) |
| Accel | x2, top speed = archetype sprint |
| Turn rate | +30%, drift cost removed |
| Sway | x0.7, terrain factor forced to 1.0 |
| Stagger | immune except headshots |
| Horse absent + full bond | whistle = immediate Majestic Return (consumes meter) |

Expected fill time: ~4–6 min of active stylish play -> 1–2 charges per 15-min match.

**Acceptance checklist:**
- [ ] Median player earns >= 1 charge/match; top quartile <= 3.
- [ ] Charge win-rate delta in duels: +15–25% (strong, not autowin).
- [ ] Bond economy can't be farmed solo (jump loops alone fill < 20% of a match's meter).
- [ ] Instant-return branch triggers correctly and feels heroic, not like skipping punishment.
- [ ] Meter legibility: testers can state their charge readiness without looking (audio tiers).

**Top risks:** (1) charge as escape button devalues dives (usage overlap) — monitor dive rate
pre/post M4; (2) bond farming routes (jump lines) distort movement; (3) full-bond instant
return may be stronger than charge — A/B which players pick.

---

## M5 — Bounty Run scoring loop

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
| Respawn | 5s, outer 70–85% of radius, min-distance placement |
| Most Wanted reveal | leader, every 60s, 10s duration (map ping + flare + birds) |
| Objective cadence | 1 event per 90s, 60s lifetime, >=150m from map edge |
| Objective payouts | moving bounty 150 / supply herd 100 / ammo wagon 80 / signal tower 50 per 10s held (max 150) / horse-buff station 50 + 60s buff |
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
encounter math below); (3) respawn ride-back time at 16p radius (~50–75s to center) may feel
punishing — candidate fix: respawn with +20% speed buff for 10s.

---

## M6 — Networked lobby: spurfire-ctl + RustScale, peer-hosted authority

**Goal:** everything above works over a real tailnet with one elected authority, 8+ players.

**Mechanics in scope:** lobby create/join via spurfire-ctl, one-use join credentials,
connectivity probe, authority election, client prediction + reconciliation for own horse,
interpolation for remotes, snapshot retention for migration, results submission.

**Tuning table:**

| Parameter | Value |
|---|---|
| Authority sim tick | 30 Hz |
| Snapshot broadcast | 20 Hz (delta-compressed) |
| Client input rate | 60 Hz |
| Remote interpolation delay | 100ms (2 snapshot intervals) |
| Prediction window | horse state predicted; shots authority-validated |
| Snapshot retention | ring buffer 10s on every peer (migration source) |
| Authority failover | detect 2s silence -> elect -> resume < 3s total |
| Election scoring | weights: direct-conn count 0.3, median RTT 0.25, worst RTT 0.15, jitter 0.1, loss 0.1, upload 0.1 |
| Match sizes validated | 6, 12, 16 |

**Acceptance checklist:**
- [ ] 8-player match completes end-to-end on a real tailnet (create -> play -> results -> teardown).
- [ ] Median RTT < 80ms direct; lobby health UI matches measured matrix.
- [ ] Kill the authority mid-match: play resumes < 3s, score/state intact (<=1 snapshot loss).
- [ ] No movement desync > 200ms peak for any client over a 15-min soak.
- [ ] Ephemeral devices and tailnet cleaned up after match (no leaked state).
- [ ] Relay fallback (DERP) playable: TTK consistency holds, sway model unaffected.

**Top risks:** (1) RustScale is alpha + sibling repo bugs — budget integration slack, log
everything, keep the one-tailnet-with-tags fallback warm; (2) authority advantage (host
literate zero-latency) skewing hit reg — measure authority-vs-peer hit% gap, target < 5%;
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

## 3. Engine-decision annex (decision explicitly open)

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

Decision gate: end of M0 spike. Bevy wins if anim pain < FFI pain; Godot wins if perf holds;
Unity/Unreal win only if 1–2 are clearly failed by the others.

---

## 4. Playtest instrumentation: tuning the Saddle Dive risk/reward

Log per dive: speed at launch, launch angle, airtime, shots fired/hit, damage dealt,
landing terrain, damage taken 0–3s post-landing, time-to-remount, outcome score delta.

| Metric | Target band | Dial if out of band |
|---|---|---|
| Dives per player per match | 2–4 | recovery 0.8s +/- 0.2s |
| Airborne hit% vs gallop baseline | +25–40% relative | airborne sway x0.6 (range 0.5–0.8) |
| Deaths within 3s of landing | 25–40% | recovery length; bad-landing penalty |
| Dive elim share of all elims | 8–15% | +25 score bonus; airborne shot cap |
| Median time-to-remount after dive | 6–10s | horse continue distance (25m) |
| Reversal dives (angle >90deg) share | 15–30% | launch cone width |
| Dive usage by match half | 2nd half >= 1st | if drops: perceived as suicide -> buff reward |
| Post-dive bond gain vs dive risk (M4) | positive expected value | +8 bond for dive elim |
| Sickness reports (camera) | ~0 | FOV kick, landing camera dip |

Kill criteria (redesign, not tune): dive elim share >25% (dominant) for 2 consecutive
milestones, or usage <1/match after two buff passes (players have voted it's not fun).

---

## Appendix: tuning-dial priority

When playtest data conflicts, turn dials in this order before touching anything structural:
(1) impulse decay times, (2) score bonuses, (3) recall reductions, (4) recovery windows,
(5) sway bases, (6) speeds, (7) radius formula constants. Lower numbers = cheaper iteration.
