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

var reload_duration := 0.0
var reload_remaining := 0.0
var marker_remaining := 0.0

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
	refresh_stats()

func _connect_once(signal_name: StringName, callable: Callable) -> void:
	if controller != null and controller.has_signal(signal_name) and not controller.is_connected(signal_name, callable):
		controller.connect(signal_name, callable)

func refresh_stats() -> void:
	if controller == null or not controller.has_method("get_weapon_stats"):
		return
	var stats := controller.call("get_weapon_stats") as Dictionary
	weapon_name.text = str(stats.get("display_name", stats.get("weapon_id", "RIFLE")))
	ammo.text = "%d | %d" % [int(stats.get("ammo_mag", stats.get("magazine", 0))), int(stats.get("ammo_reserve", stats.get("reserve", 0)))]
	set_spread(float(stats.get("effective_spread_deg", stats.get("base_spread_deg", 0.8))), str(stats.get("gait", "steady")), false, false)

func set_spread(spread_deg: float, movement_state: String, hostile: bool, in_range: bool = true) -> void:
	crosshair.set("gap", clampf(8.0 + spread_deg * 7.0, 9.0, 38.0))
	crosshair.set("tint", Color("ef4f58") if hostile else (Color("e7edf0") if in_range else Color("858a91")))
	crosshair.queue_redraw()
	state.text = movement_state.to_upper()

func set_gait(value: String) -> void:
	gait.text = value.to_upper()

func begin_reload(duration: float) -> void:
	reload_duration = maxf(duration, 0.001)
	reload_remaining = reload_duration
	reload_ring.visible = true
	reload_ring.value = 0.0

func cancel_reload() -> void:
	reload_remaining = 0.0
	reload_ring.visible = false

func _process(delta: float) -> void:
	if reload_remaining > 0.0:
		reload_remaining = maxf(0.0, reload_remaining - delta)
		reload_ring.value = 100.0 * (1.0 - reload_remaining / reload_duration)
		reload_time.text = "%.1fs" % reload_remaining
		if reload_remaining == 0.0:
			reload_ring.visible = false
	if marker_remaining > 0.0:
		marker_remaining -= delta
		if marker_remaining <= 0.0:
			hit_marker.visible = false

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
