extends Control
class_name CombatHud

@export var controller: Node
@onready var crosshair: Control = %Crosshair
@onready var weapon_name: Label = %WeaponName
@onready var ammo: Label = %Ammo
@onready var state: Label = %State
@onready var gait: Label = %Gait
@onready var reload_ring: ProgressBar = %ReloadRing
@onready var reload_time: Label = %ReloadTime
@onready var hit_marker: Label = %HitMarker

var marker_remaining := 0.0
var feedback_remaining := 0.0
var _base_state_text := "STEADY"
var _dive_ready := false
var _dive_clamped := false
var _dive_requested_angle := 0.0
var _dive_clamped_angle := 0.0

func _ready() -> void:
	if controller != null:
		bind_controller(controller)
	reload_ring.visible = false
	hit_marker.visible = false

func bind_controller(value: Node) -> void:
	controller = value
	_connect_once(&"weapon_changed", _on_weapon_changed)
	_connect_once(&"ammo_changed", _on_ammo_changed)
	_connect_once(&"hit_confirmed", _on_hit_confirmed)
	_connect_once(&"reload_started", _on_reload_started)
	_connect_once(&"reload_progressed", _on_reload_progressed)
	_connect_once(&"reload_completed", _on_reload_completed)
	_connect_once(&"reload_rejected", _on_reload_rejected)
	_connect_once(&"fire_rejected", _on_fire_rejected)
	var gameplay := _find_gameplay()
	if gameplay and gameplay.has_signal(&"dive_preview_updated"):
		var callback := Callable(self, "_on_dive_preview_updated")
		if not gameplay.is_connected(&"dive_preview_updated", callback):
			gameplay.connect(&"dive_preview_updated", callback)
	refresh_stats()

func _connect_once(signal_name: StringName, callable: Callable) -> void:
	if controller != null and controller.has_signal(signal_name) and not controller.is_connected(signal_name, callable):
		controller.connect(signal_name, callable)

func _find_gameplay() -> Node:
	if controller == null:
		return null
	var rider := controller.get_parent()
	var course := rider.get_parent() if rider else null
	return course.get_node_or_null("M2Gameplay") if course else null

func refresh_stats() -> void:
	if controller == null or not controller.has_method("get_weapon_stats"):
		return
	var stats := controller.call("get_weapon_stats") as Dictionary
	weapon_name.text = str(stats.get("display_name", stats.get("weapon_id", "RIFLE")))
	ammo.text = "%d | %d" % [
		int(stats.get("ammo_mag", stats.get("magazine", 0))),
		int(stats.get("ammo_reserve", stats.get("reserve", 0)))
	]
	set_spread(
		float(stats.get("effective_spread_deg", stats.get("base_spread_deg", 0.8))),
		str(stats.get("gait", "steady")),
		false,
		false
	)
	if bool(stats.get("is_reloading", false)):
		_on_reload_started(int(controller.get("current_tick")), int(stats.get("reload_ticks", 1)))
		_on_reload_progressed(
			int(controller.get("current_tick")),
			float(stats.get("reload_progress", 0.0)),
			0,
			int(stats.get("reload_ticks", 1))
		)

func set_spread(spread_deg: float, movement_state: String, hostile: bool, in_range: bool = true) -> void:
	crosshair.set("gap", clampf(8.0 + spread_deg * 7.0, 9.0, 38.0))
	crosshair.set("tint", Color("ef4f58") if hostile else (Color("e7edf0") if in_range else Color("858a91")))
	crosshair.queue_redraw()
	_base_state_text = movement_state.to_upper()
	_refresh_state_text()

func set_gait(value: String) -> void:
	gait.text = value.to_upper()

func _process(delta: float) -> void:
	if feedback_remaining > 0.0:
		feedback_remaining = maxf(0.0, feedback_remaining - delta)
		if feedback_remaining == 0.0:
			reload_ring.visible = bool(controller.get("is_reloading")) if controller else false
			reload_ring.modulate = Color.WHITE
			_refresh_state_text()
	if marker_remaining > 0.0:
		marker_remaining -= delta
		if marker_remaining <= 0.0:
			hit_marker.visible = false

func _refresh_state_text() -> void:
	if feedback_remaining > 0.0:
		return
	if _dive_ready:
		if _dive_clamped:
			state.text = "DIVE CLAMP %+.0f° → %+.0f°" % [
				_dive_requested_angle,
				_dive_clamped_angle
			]
			crosshair.set("tint", Color("ffd166"))
		else:
			state.text = "DIVE READY • E TOWARD CROSSHAIR"
			crosshair.set("tint", Color("8fe388"))
		crosshair.queue_redraw()
	else:
		state.text = _base_state_text

func _on_dive_preview_updated(
	ready: bool,
	requested_angle: float,
	clamped_angle: float,
	was_clamped: bool,
	_direction: Vector3
) -> void:
	_dive_ready = ready
	_dive_requested_angle = requested_angle
	_dive_clamped_angle = clamped_angle
	_dive_clamped = was_clamped
	_refresh_state_text()

func _on_reload_started(_tick: int, _required_ticks: int) -> void:
	feedback_remaining = 0.0
	reload_ring.modulate = Color.WHITE
	reload_ring.visible = true
	reload_ring.value = 0.0
	reload_time.text = "RELOADING  0%"
	state.text = "RELOADING"

func _on_reload_progressed(
	_tick: int,
	progress: float,
	_active_ticks: int,
	_required_ticks: int
) -> void:
	if progress < 1.0:
		reload_ring.visible = true
	var percent := clampf(progress, 0.0, 1.0) * 100.0
	reload_ring.value = percent
	reload_time.text = "RELOADING  %.0f%%" % percent

func _on_reload_completed(_tick: int, mag: int, reserve: int) -> void:
	_on_ammo_changed(mag, reserve)
	reload_ring.value = 100.0
	reload_ring.visible = false
	reload_time.text = "RELOAD COMPLETE"
	state.text = "RELOAD COMPLETE"
	feedback_remaining = 0.45

func _on_reload_rejected(_tick: int, reason: String) -> void:
	_show_reload_rejection(reason)

func show_reload_rejection(reason: String) -> void:
	# CombatPlayer uses this only as a compatibility fallback when a controller
	# cannot expose the native rejection signal.
	_show_reload_rejection(reason)

func _show_reload_rejection(reason: String) -> void:
	var readable := reason.replace("_", " ").to_upper()
	reload_ring.modulate = Color("ef4f58")
	reload_ring.value = 100.0
	reload_ring.visible = true
	reload_time.text = "CAN'T RELOAD — %s" % readable
	state.text = "CAN'T RELOAD — %s" % readable
	feedback_remaining = 0.9

func _on_fire_rejected(_tick: int, reason: String) -> void:
	if reason in ["empty_magazine", "no_ammo"]:
		state.text = "R — RELOAD"
		feedback_remaining = 0.7

func _on_weapon_changed(_weapon_id: Variant) -> void:
	refresh_stats()

func _on_ammo_changed(mag: int, reserve: int) -> void:
	ammo.text = "%d | %d" % [mag, reserve]
	ammo.modulate = Color("ef4f58") if mag <= 0 else (Color("ffd166") if mag <= 6 else Color.WHITE)

func _on_hit_confirmed(_target_id: Variant, hit_zone: Variant, damage: Variant) -> void:
	var zone := str(hit_zone).to_lower()
	hit_marker.text = "◆  ◆\n  X\n◆  ◆"
	hit_marker.modulate = Color("ffd166") if zone == "head" else Color.WHITE
	if float(damage) >= 100.0:
		hit_marker.modulate = Color("ef4f58")
	hit_marker.visible = true
	marker_remaining = 0.16
