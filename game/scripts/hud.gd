extends CanvasLayer

@export var horse: Node
@export var rider: Node3D
var _log: FileAccess
const GAITS := ["IDLE", "WALK", "TROT", "GALLOP"]
const CREAM := Color("f5e9d0")
const DARK := Color("2b1d12")
const TEAL := Color("3fb6c9")
const RED := Color("c44536")

var _tack_panel: PanelContainer
var _gait_label: Label
var _speed_label: Label
var _speed_bar: ProgressBar
var _prompt: Label
var _debug_visible := false
var _dive_ready := false
var _dive_clamped := false
var _mouse_delta := Vector2.ZERO
var _debug_details := ""

func _ready() -> void:
	$Panel.visible = false
	$Help.visible = false
	$Panel.mouse_filter = Control.MOUSE_FILTER_IGNORE
	$Help.mouse_filter = Control.MOUSE_FILTER_IGNORE
	_build_player_hud()
	if horse and horse.has_signal("telemetry_updated"):
		horse.telemetry_updated.connect(_on_telemetry)
	var gameplay := get_parent().get_node_or_null("M2Gameplay")
	if gameplay and gameplay.has_signal(&"dive_preview_updated"):
		gameplay.dive_preview_updated.connect(_on_dive_preview)
	var camera := get_parent().get_node_or_null("CameraRig")
	if camera and camera.has_signal(&"mouse_delta_sampled"):
		camera.mouse_delta_sampled.connect(func(value: Vector2): _mouse_delta = value)
	_log = FileAccess.open("user://m0_telemetry.csv", FileAccess.WRITE)
	if _log:
		_log.store_line("time_ms,speed_mps,speed_kmh,gait,slope_angle_deg,surface,x,y,z,is_airborne")

func _unhandled_input(event: InputEvent) -> void:
	if event.is_action_pressed(&"toggle_diagnostics"):
		_debug_visible = not _debug_visible
		$Panel.visible = _debug_visible
		$Help.visible = _debug_visible
		get_viewport().set_input_as_handled()

func _process(_delta: float) -> void:
	var hint := $Panel/Margin/VBox/RemountHint as Label
	var retrievable := horse != null and bool(horse.get("is_retrievable"))
	var on_foot := rider != null and int(rider.get("stance_id")) == 6
	hint.visible = retrievable and on_foot
	if retrievable and on_foot:
		var offset := rider.global_position - (horse as Node3D).global_position
		var distance := Vector2(offset.x, offset.z).length()
		var text := "E — REMOUNT" if distance <= 3.0 else "HORSE READY  •  %.1f m" % distance
		hint.text = text
		_prompt.text = text
	elif _dive_ready:
		_prompt.text = "E — SADDLE DIVE  •  75° FAN%s" % ("  •  CLAMPED" if _dive_clamped else "")
		_prompt.modulate = RED if _dive_clamped else CREAM
	else:
		_prompt.text = "W — RIDE  •  A / D — REINS"
		_prompt.modulate = CREAM
	if _debug_visible:
		$Panel/Margin/VBox/Details.text = _debug_details + "   mouse Δ %.1f, %.1f" % [_mouse_delta.x, _mouse_delta.y]
	_mouse_delta = Vector2.ZERO

func _on_telemetry(data: Dictionary) -> void:
	var gait_index := clampi(int(data.get("gait", 0)), 0, GAITS.size() - 1)
	var speed := float(data.get("speed_mps", 0.0))
	var kmh := float(data.get("speed_kmh", speed * 3.6))
	var slope := float(data.get("slope_angle_deg", 0.0))
	_gait_label.text = _gait_pips(gait_index) + "  " + GAITS[gait_index]
	_speed_label.text = "%4.1f m/s" % speed
	_speed_bar.value = clampf(float(data.get("speed_fraction", 0.0)) * 100.0, 0.0, 100.0)
	$Panel/Margin/VBox/Readout.text = "%s  |  %.2f m/s  %.1f km/h  |  slope %.1f°" % [GAITS[gait_index], speed, kmh, slope]
	_debug_details = "surface: %s   airborne: %s   turn radius: %.1f m" % [data.get("surface", "flat"), data.get("is_airborne", false), float(data.get("turn_radius_m", 0.0))]
	$Panel/Margin/VBox/Details.text = _debug_details
	$Panel/Margin/VBox/SpeedGraph.add_sample(speed)
	if _log:
		var position: Vector3 = data.get("position", Vector3.ZERO)
		_log.store_csv_line(PackedStringArray([str(Time.get_ticks_msec()), str(speed), str(kmh), str(gait_index), str(slope), str(data.get("surface", "flat")), str(position.x), str(position.y), str(position.z), str(data.get("is_airborne", false))]))
		_log.flush()

func _on_dive_preview(ready: bool, _requested: float, _clamped: float, was_clamped: bool, _direction: Vector3) -> void:
	_dive_ready = ready
	_dive_clamped = was_clamped

func _gait_pips(active: int) -> String:
	var result := ""
	for index in 4:
		result += "■ " if index <= active else "□ "
	return result.strip_edges()

func _build_player_hud() -> void:
	_tack_panel = PanelContainer.new()
	_tack_panel.name = "TackCluster"
	_tack_panel.set_anchors_preset(Control.PRESET_BOTTOM_LEFT)
	_tack_panel.position = Vector2(24, -116)
	_tack_panel.size = Vector2(285, 92)
	_tack_panel.mouse_filter = Control.MOUSE_FILTER_IGNORE
	var style := StyleBoxFlat.new()
	style.bg_color = Color(DARK, 0.9)
	style.corner_radius_top_left = 9
	style.corner_radius_top_right = 9
	style.corner_radius_bottom_left = 9
	style.corner_radius_bottom_right = 9
	style.content_margin_left = 16
	style.content_margin_right = 16
	style.content_margin_top = 12
	style.content_margin_bottom = 12
	_tack_panel.add_theme_stylebox_override("panel", style)
	var stack := VBoxContainer.new()
	_tack_panel.add_child(stack)
	_gait_label = Label.new()
	_gait_label.add_theme_color_override("font_color", CREAM)
	_gait_label.add_theme_font_size_override("font_size", 17)
	stack.add_child(_gait_label)
	var speed_row := HBoxContainer.new()
	stack.add_child(speed_row)
	_speed_bar = ProgressBar.new()
	_speed_bar.custom_minimum_size = Vector2(170, 18)
	_speed_bar.show_percentage = false
	_speed_bar.add_theme_color_override("font_color", TEAL)
	speed_row.add_child(_speed_bar)
	_speed_label = Label.new()
	_speed_label.add_theme_color_override("font_color", CREAM)
	speed_row.add_child(_speed_label)
	add_child(_tack_panel)
	_prompt = Label.new()
	_prompt.name = "PromptRail"
	_prompt.set_anchors_preset(Control.PRESET_CENTER_BOTTOM)
	_prompt.position = Vector2(-250, -68)
	_prompt.size = Vector2(500, 38)
	_prompt.horizontal_alignment = HORIZONTAL_ALIGNMENT_CENTER
	_prompt.add_theme_color_override("font_color", CREAM)
	_prompt.add_theme_color_override("font_shadow_color", DARK)
	_prompt.add_theme_constant_override("shadow_offset_x", 2)
	_prompt.add_theme_constant_override("shadow_offset_y", 2)
	_prompt.add_theme_font_size_override("font_size", 18)
	_prompt.mouse_filter = Control.MOUSE_FILTER_IGNORE
	add_child(_prompt)
