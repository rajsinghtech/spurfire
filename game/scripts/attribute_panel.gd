class_name SpurfireAttributePanel
extends PanelContainer

const ARCHETYPE_NAMES := ["COURSER", "WARHORSE", "MUSTANG"]
const DESCRIPTIONS := [
	"Fastest legs on the range, bruises easy.",
	"Slow to start, impossible to stop.",
	"Turns on a bit and laughs at bad ground.",
]
const RATINGS := [
	[5, 5, 3, 3, 1],
	[3, 1, 1, 1, 5],
	[4, 4, 5, 5, 3],
]
const STAT_KEYS := ["speed", "acceleration", "turning", "jump / rough", "vitality"]
const FALLBACK_STATS := [
	{"walk_mps": 2.0, "trot_mps": 5.0, "gallop_mps": 14.5, "sprint_mps": 16.5, "accel_0_to_gallop_s": 3.0, "turn_walk_deg_s": 140.0, "turn_gallop_deg_s": 60.0, "drift_deg_s": 120.0, "jump_apex_m": 1.8, "jump_airtime_s": 0.7, "terrain_scrub": 0.90, "terrain_mud": 0.70, "terrain_riverbed": 0.95, "terrain_recovery_s": 2.0, "max_vitality": 200.0, "stagger_threshold": 40.0, "sidestep_mps": 1.0, "sidestep_ramp_s": 0.25},
	{"walk_mps": 1.8, "trot_mps": 4.2, "gallop_mps": 12.0, "sprint_mps": 13.5, "accel_0_to_gallop_s": 5.0, "turn_walk_deg_s": 120.0, "turn_gallop_deg_s": 45.0, "drift_deg_s": 90.0, "jump_apex_m": 1.2, "jump_airtime_s": 0.5, "terrain_scrub": 0.90, "terrain_mud": 0.75, "terrain_riverbed": 0.95, "terrain_recovery_s": 2.5, "max_vitality": 320.0, "stagger_threshold": 90.0, "sidestep_mps": 0.8, "sidestep_ramp_s": 0.35},
	{"walk_mps": 1.9, "trot_mps": 4.6, "gallop_mps": 13.0, "sprint_mps": 14.5, "accel_0_to_gallop_s": 3.5, "turn_walk_deg_s": 150.0, "turn_gallop_deg_s": 80.0, "drift_deg_s": 150.0, "jump_apex_m": 2.2, "jump_airtime_s": 0.8, "terrain_scrub": 0.95, "terrain_mud": 0.80, "terrain_riverbed": 0.95, "terrain_recovery_s": 1.5, "max_vitality": 250.0, "stagger_threshold": 60.0, "sidestep_mps": 1.2, "sidestep_ramp_s": 0.15},
]

@export_range(0, 2, 1) var archetype_id := 0
var _stats: Dictionary = {}

@onready var title_label: Label = %ArchetypeName
@onready var description_label: Label = %Description
@onready var bars: Array[ProgressBar] = [%SpeedBar, %AccelerationBar, %TurningBar, %JumpBar, %VitalityBar]
@onready var exact_label: Label = %ExactStats

func _ready() -> void:
	set_archetype(archetype_id, _stats)

func set_archetype(id: int, stats: Dictionary = {}) -> void:
	archetype_id = clampi(id, 0, 2)
	_stats = stats.duplicate() if not stats.is_empty() else FALLBACK_STATS[archetype_id].duplicate()
	if not is_node_ready():
		return
	title_label.text = ARCHETYPE_NAMES[archetype_id]
	description_label.text = DESCRIPTIONS[archetype_id]
	for index in bars.size():
		bars[index].value = RATINGS[archetype_id][index]
		bars[index].tooltip_text = "%s: %d of 5" % [STAT_KEYS[index].capitalize(), RATINGS[archetype_id][index]]
	exact_label.text = _format_exact_stats(_stats)

func set_exact_stats_visible(visible: bool) -> void:
	%ExactSection.visible = visible

func _format_exact_stats(stats: Dictionary) -> String:
	return "SPEED  %.1f / %.1f / %.1f / %.1f m/s\nACCEL  %.1fs     TURN  %.0f°→%.0f°/s\nJUMP  %.1fm / %.1fs     ROUGH  %.2f / %.2f / %.2f\nRECOVERY  %.1fs     VITALITY  %.0f     STAGGER  %.0f\nSIDESTEP  %.1f m/s (%.2fs ramp)" % [
		float(stats.get("walk_mps", 0.0)), float(stats.get("trot_mps", 0.0)),
		float(stats.get("gallop_mps", 0.0)), float(stats.get("sprint_mps", 0.0)),
		float(stats.get("accel_0_to_gallop_s", 0.0)), float(stats.get("turn_walk_deg_s", 0.0)),
		float(stats.get("turn_gallop_deg_s", 0.0)), float(stats.get("jump_apex_m", 0.0)),
		float(stats.get("jump_airtime_s", 0.0)), float(stats.get("terrain_scrub", 0.0)),
		float(stats.get("terrain_mud", 0.0)), float(stats.get("terrain_riverbed", 0.0)),
		float(stats.get("terrain_recovery_s", 0.0)), float(stats.get("max_vitality", 0.0)),
		float(stats.get("stagger_threshold", 0.0)), float(stats.get("sidestep_mps", 0.0)),
		float(stats.get("sidestep_ramp_s", 0.0)),
	]
