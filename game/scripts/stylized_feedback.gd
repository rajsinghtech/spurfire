class_name SpurfireStylizedFeedback
extends Control

@export var horse: Node
@export var rider: Node
@export_range(0.0, 1.0, 0.05) var maximum_opacity := 0.26
@export_range(4, 24, 1) var maximum_line_count := 14
@export var line_color := Color("3fb6c9")
@export var dust_color := Color("d9a05b")

var _speed_fraction := 0.0
var _lateral_fraction := 0.0
var _dust_amount := 0.0
var _phase := 0.0
var _was_airborne := false
var _landing_flash := 0.0
var _bad_landing_flash := 0.0
var _majestic_charge_active := false

func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	if horse and horse.has_signal("telemetry_updated"):
		horse.connect("telemetry_updated", apply_telemetry)
	if rider and rider.has_signal("dive_landed"):
		rider.connect("dive_landed", _on_dive_landed)
	set_process(true)

func set_horse(value: Node) -> void:
	horse = value
	if is_node_ready() and horse and horse.has_signal("telemetry_updated") and not horse.is_connected("telemetry_updated", apply_telemetry):
		horse.connect("telemetry_updated", apply_telemetry)

func apply_telemetry(data: Dictionary) -> void:
	var speed := absf(float(data.get("speed_mps", 0.0)))
	var sprint := maxf(float(data.get("sprint_mps", data.get("max_speed_mps", 16.5))), 0.1)
	_speed_fraction = clampf(float(data.get("speed_fraction", speed / sprint)), 0.0, 1.0)
	var sidestep_cap := maxf(float(data.get("sidestep_mps", 1.2)), 0.1)
	_lateral_fraction = clampf(absf(float(data.get("lateral_speed_mps", 0.0))) / sidestep_cap, 0.0, 1.0)
	var surface := String(data.get("surface", "flat")).to_lower()
	var rough_surface := surface.contains("rough") or surface.contains("scrub") or surface.contains("mud") or surface.contains("river")
	_dust_amount = clampf((_speed_fraction - 0.18) * 0.65 + (0.25 if rough_surface else 0.0), 0.0, 1.0)
	var airborne := bool(data.get("is_airborne", false))
	if _was_airborne and not airborne:
		_landing_flash = 1.0
	_was_airborne = airborne
	queue_redraw()

func _on_dive_landed(_dive_id: int, bad: bool, _slope: float, _terrain: String) -> void:
	_landing_flash = 1.0
	_bad_landing_flash = 1.0 if bad else 0.0
	queue_redraw()

func set_majestic_charge_active(active: bool) -> void:
	_majestic_charge_active = active
	queue_redraw()

func _process(delta: float) -> void:
	_phase = fmod(_phase + delta * lerpf(0.4, 2.4, _speed_fraction), 1.0)
	_landing_flash = move_toward(_landing_flash, 0.0, delta * 2.8)
	_bad_landing_flash = move_toward(_bad_landing_flash, 0.0, delta * 1.8)
	if _speed_fraction > 0.12 or _lateral_fraction > 0.05 or _landing_flash > 0.01 or _bad_landing_flash > 0.01:
		queue_redraw()

func _draw() -> void:
	if size.x <= 1.0 or size.y <= 1.0:
		return
	_draw_speed_lines()
	_draw_dust_puffs()

func _draw_speed_lines() -> void:
	var strength := maxf(smoothstep(0.42, 1.0, _speed_fraction), 0.9 if _majestic_charge_active else 0.0)
	var count := int(round(strength * maximum_line_count))
	for index in count:
		var seed := fmod(float(index) * 0.6180339 + _phase, 1.0)
		var side := -1.0 if index % 2 == 0 else 1.0
		var x := size.x * (0.5 + side * lerpf(0.20, 0.48, seed))
		var y := size.y * fmod(float(index) * 0.371 + _phase * 0.45, 0.88)
		var length := lerpf(18.0, 70.0, strength) * lerpf(0.7, 1.2, seed)
		var direction := Vector2(side * 0.18, 1.0).normalized()
		var color := Color("ffd166") if _majestic_charge_active else line_color
		color.a = maximum_opacity * strength * lerpf(0.45, 1.0, seed)
		draw_line(Vector2(x, y), Vector2(x, y) + direction * length, color, lerpf(2.0, 4.0, strength), true)

func _draw_dust_puffs() -> void:
	var amount := maxf(_dust_amount, _landing_flash)
	if amount <= 0.01:
		return
	var center := Vector2(size.x * (0.5 + _lateral_fraction * 0.06), size.y * 0.88)
	for index in 7:
		var angle := float(index) / 7.0 * PI + PI
		var drift := Vector2(cos(angle) * 105.0, sin(angle) * 24.0)
		var pulse := fmod(_phase * 1.7 + float(index) * 0.19, 1.0)
		var radius := lerpf(5.0, 17.0, pulse) * amount
		var color := dust_color.lerp(Color("ff5c35"), _bad_landing_flash)
		color.a = (1.0 - pulse) * amount * lerpf(0.28, 0.48, _bad_landing_flash)
		draw_circle(center + drift * pulse, radius, color)
