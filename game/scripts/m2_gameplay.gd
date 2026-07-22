extends Node
class_name M2Gameplay

signal simulation_tick_advanced(tick: int)
signal dive_preview_updated(
	ready: bool,
	requested_angle_degrees: float,
	clamped_angle_degrees: float,
	was_clamped: bool,
	clamped_direction: Vector3
)

@export var rider: CharacterBody3D
@export var horse: CharacterBody3D
@export var aim_camera: Camera3D
@export var weapon_controller: Node3D
@export var combat_input: Node
@export var replication: Node

const MIN_DIVE_SPEED_MPS := 8.0
const TELEMETRY_SCHEMA_VERSION := 1
const DIVE_TELEMETRY_KEYS := [
	"schema_version", "authority_epoch", "actor", "dive_id", "launch_tick",
	"launch_weapon", "launch_gait", "prelaunch_velocity_mmps", "prelaunch_speed_mmps",
	"requested_angle_millidegrees", "clamped_angle_millidegrees",
	"direction_was_clamped", "horizontal_impulse_mmps", "resulting_planar_speed_mmps",
	"resulting_total_speed_mmps", "vertical_pop_mmps", "launch_height_mm",
	"nominal_airtime_ticks", "landing_tick", "airtime_ticks", "shot_attempts",
	"shots_fired", "shots_hit", "headshots", "reversal_hits", "damage_dealt",
	"landing_terrain", "landing_slope_millidegrees", "landing_outcome",
	"landing_damage", "damage_taken_landing_through_3s", "death_tick",
	"death_within_3s", "remount_tick", "time_to_remount_ticks", "censor_reason",
]

var simulation_tick := 0
var dive_ready := false
var dive_direction_clamped := false
var dive_requested_angle_degrees := 0.0
var dive_clamped_angle_degrees := 0.0
var dive_preview_direction := Vector3.FORWARD
var telemetry_log_path := ""

var _last_stance_id := 1
var _stance_changed_this_tick := false
var _course_reset_requested := false
var _telemetry_file: FileAccess
var _session_id := ""
var _session_closed := false
var _persisted_dive_keys: Dictionary = {}
var _presentation_input_enabled := false
var _capture_button_blocked := false

func _ready() -> void:
	process_physics_priority = -100
	if rider:
		_last_stance_id = int(rider.get("stance_id"))
		rider.stance_changed.connect(_on_stance_changed)
		if rider.has_signal(&"dive_started"):
			rider.dive_started.connect(_on_dive_started)
		if rider.has_signal(&"dive_telemetry_finalized"):
			rider.dive_telemetry_finalized.connect(_on_dive_telemetry_finalized)
	_open_telemetry_session()

func _physics_process(_delta: float) -> void:
	if rider == null or horse == null or weapon_controller == null:
		return
	simulation_tick += 1
	_stance_changed_this_tick = false

	if _capture_button_blocked and not Input.is_action_pressed(&"combat_fire"):
		_capture_button_blocked = false
	var accepts_input := _presentation_input_enabled and not _capture_button_blocked

	if (accepts_input and Input.is_action_just_pressed(&"reset_horse")) or _course_reset_requested:
		_course_reset_requested = false
		rider.call("reset_rider", simulation_tick)
		horse.call("set_external_simulation_tick", simulation_tick)
		_reset_presentation_after_teleport()
		_advance_replication(true)
		simulation_tick_advanced.emit(simulation_tick)
		return

	var chosen_direction := -aim_camera.global_basis.z if aim_camera else -rider.global_basis.z
	_update_dive_preview(chosen_direction)
	var move_input := Vector2.ZERO
	if accepts_input:
		move_input = Vector2(
			Input.get_axis(&"steer_left", &"steer_right"),
			Input.get_axis(&"move_back", &"move_forward")
		)
	var weapon_id := int(weapon_controller.get("weapon_id"))
	# Install current horse handling while combat still holds the previous
	# authoritative stance. The Rust rider transition then opens/closes dive
	# context atomically before same-tick fire or reload.
	if not _install_combat_context():
		push_error("M2 combat context rejected before tick %d" % simulation_tick)
		return
	rider.call(
		"advance_tick",
		simulation_tick,
		accepts_input and Input.is_action_just_pressed(&"combat_interact"),
		chosen_direction,
		move_input,
		weapon_id
	)
	if not bool(weapon_controller.call("advance_to_tick", simulation_tick)):
		push_error("M2 combat clock rejected tick %d" % simulation_tick)
		return
	if combat_input and combat_input.has_method("process_combat_tick"):
		combat_input.call("process_combat_tick", simulation_tick)

	# Collision feedback is intentionally after fire/reload. This preserves the
	# locked pre-contact shot / post-contact recovery ordering.
	horse.call("set_external_simulation_tick", simulation_tick)
	rider.call("resolve_motion", simulation_tick)
	rider.call("settle_observations_through", simulation_tick)
	var final_stance := int(rider.get("stance_id"))
	if final_stance != _last_stance_id:
		_stance_changed_this_tick = true
		_last_stance_id = final_stance
	_advance_replication(_stance_changed_this_tick)
	simulation_tick_advanced.emit(simulation_tick)

func _install_combat_context() -> bool:
	var stats := horse.call("get_archetype_stats") as Dictionary
	var stance_id := int(rider.get("stance_id"))
	# Combat DiveId exists iff fire is currently in SaddleDiveAirborne. The
	# movement kernel keeps its DiveId through recovery for telemetry/remount.
	var combat_dive_id := int(rider.get("dive_id")) if stance_id == 3 else -1
	return bool(weapon_controller.call(
		"set_rider_context",
		simulation_tick,
		stance_id,
		combat_dive_id,
		int(horse.get("current_gait")),
		float(horse.get("speed_mps")),
		float(stats.get("gallop_mps", 13.0)),
		float(horse.get("yaw_rate_degrees")),
		false,
		_presentation_input_enabled and not _capture_button_blocked and Input.is_action_pressed(&"combat_aim"),
		false
	))

func _update_dive_preview(chosen_direction: Vector3) -> void:
	var speed := Vector2(horse.velocity.x, horse.velocity.z).length()
	dive_ready = (
		int(rider.get("stance_id")) == 1
		and horse.is_on_floor()
		and speed >= MIN_DIVE_SPEED_MPS
	)
	var preview := weapon_controller.call(
		"preview_dive_direction",
		horse.velocity,
		chosen_direction
	) as Dictionary
	dive_requested_angle_degrees = float(preview.get("requested_angle_degrees", 0.0))
	dive_clamped_angle_degrees = float(preview.get("clamped_angle_degrees", 0.0))
	dive_direction_clamped = bool(preview.get("direction_was_clamped", false))
	dive_preview_direction = preview.get("clamped_direction", Vector3.FORWARD)
	dive_preview_updated.emit(
		dive_ready,
		dive_requested_angle_degrees,
		dive_clamped_angle_degrees,
		dive_direction_clamped,
		dive_preview_direction
	)

func request_course_reset() -> void:
	_course_reset_requested = true

func set_presentation_input_enabled(enabled: bool, suppress_button := false) -> void:
	_presentation_input_enabled = enabled
	_capture_button_blocked = enabled and suppress_button

func _reset_presentation_after_teleport() -> void:
	var camera_rig := get_parent().get_node_or_null("CameraRig")
	if camera_rig and camera_rig.has_method("reset_follow"):
		camera_rig.call_deferred("reset_follow")
	var pose := rider.get_node_or_null("RiderProxy")
	if pose and pose.has_method("reset_pose_interpolation"):
		pose.call_deferred("reset_pose_interpolation")

func _advance_replication(stance_changed: bool) -> void:
	if replication and replication.has_method("advance_shared_tick"):
		replication.call("advance_shared_tick", simulation_tick, stance_changed)

func _on_stance_changed(_previous_id: int, current_id: int, _tick: int, _dive_id: int) -> void:
	if current_id != _last_stance_id:
		_stance_changed_this_tick = true

func _open_telemetry_session() -> void:
	var crypto := Crypto.new()
	_session_id = crypto.generate_random_bytes(16).hex_encode()
	var logs_path := ProjectSettings.globalize_path("user://logs")
	var error := DirAccess.make_dir_recursive_absolute(logs_path)
	if error != OK:
		push_warning("M2 telemetry log directory unavailable: %s" % error_string(error))
		return
	var started_unix := int(Time.get_unix_time_from_system())
	telemetry_log_path = "user://logs/m2-%d-%s.jsonl" % [started_unix, _session_id.left(12)]
	_telemetry_file = FileAccess.open(telemetry_log_path, FileAccess.WRITE)
	if _telemetry_file == null:
		push_warning("M2 telemetry log unavailable: %s" % FileAccess.get_open_error())
		return
	_store_telemetry_record({
		"event_type": "session_started",
		"timestamp_ms": int(Time.get_unix_time_from_system() * 1000.0),
		"build_version": str(ProjectSettings.get_setting("application/config/version", "unknown")),
		"simulation_hz": int(Engine.physics_ticks_per_second),
	})

func _on_dive_started(dive_id: int, _launch_velocity: Vector3, _clamped_angle_degrees: float) -> void:
	_store_telemetry_record({
		"event_type": "dive_started",
		"dive_id": dive_id,
	})

func _on_dive_telemetry_finalized(row: Dictionary) -> void:
	var key := "%s:%s:%s" % [
		str(row.get("authority_epoch", 0)),
		str(row.get("actor", "")),
		str(row.get("dive_id", -1)),
	]
	if _persisted_dive_keys.has(key):
		return
	_persisted_dive_keys[key] = true
	var record := {
		"event_type": (
			"dive_censored" if not str(row.get("censor_reason", "")).is_empty()
			else "dive_finalized"
		),
	}
	for field in DIVE_TELEMETRY_KEYS:
		if row.has(field):
			record[field] = _json_safe_value(row[field])
	_store_telemetry_record(record)

func _json_safe_value(value: Variant) -> Variant:
	if value is Vector2:
		return [value.x, value.y]
	if value is Vector2i:
		return [value.x, value.y]
	if value is Vector3:
		return [value.x, value.y, value.z]
	if value is Vector3i:
		return [value.x, value.y, value.z]
	return value

func _store_telemetry_record(record: Dictionary) -> void:
	if _telemetry_file == null:
		return
	record["schema_version"] = TELEMETRY_SCHEMA_VERSION
	record["session_id"] = _session_id
	record["build_commit"] = str(ProjectSettings.get_setting(
		"application/config/build_commit",
		"development"
	))
	if (
		rider and is_instance_valid(rider) and not record.has("actor")
		and str(record.get("event_type", "")) != "session_started"
	):
		record["actor"] = str(rider.get("actor_id"))
	_telemetry_file.store_line(JSON.stringify(record))
	_telemetry_file.flush()

func _close_telemetry_session(reason: String) -> void:
	if _session_closed:
		return
	_session_closed = true
	if rider and is_instance_valid(rider) and rider.has_method("end_match"):
		rider.call("end_match", simulation_tick)
	_store_telemetry_record({
		"event_type": "session_ended",
		"timestamp_ms": int(Time.get_unix_time_from_system() * 1000.0),
		"reason": reason,
	})
	if _telemetry_file:
		_telemetry_file.close()
		_telemetry_file = null

func _notification(what: int) -> void:
	if what == NOTIFICATION_WM_CLOSE_REQUEST:
		_close_telemetry_session("window_close")
	elif what == NOTIFICATION_PREDELETE:
		_close_telemetry_session("scene_close")
