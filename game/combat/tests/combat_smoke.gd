extends Node

class FakeWeaponController:
	extends Node3D
	signal weapon_changed(weapon_id)
	signal ammo_changed(mag, reserve)
	signal shot_fired(tick, weapon_id)
	signal hit_confirmed(target_id, hit_zone, damage)
	var weapon_id := 0
	var fire_calls: Array = []
	var reload_calls := 0
	var mag := 30
	var reserve := 120

	func equip_weapon(id) -> bool:
		weapon_id = int(id)
		weapon_changed.emit(weapon_id)
		return true

	func request_fire(origin: Vector3, direction: Vector3, tick: int) -> bool:
		fire_calls.append([origin, direction, tick])
		mag -= 1
		ammo_changed.emit(mag, reserve)
		shot_fired.emit(tick, weapon_id)
		return true

	func request_reload() -> bool:
		reload_calls += 1
		return true

	func get_weapon_stats() -> Dictionary:
		return {"weapon_id": weapon_id, "display_name": "SF-C30 Dustwalker", "magazine": 30, "reserve": 120, "ammo_mag": mag, "ammo_reserve": reserve, "base_spread_deg": 0.8}

const SCENES := {
	"hud": "res://ui/combat/combat_hud.tscn",
	"rifle": "res://combat/mounted_rifle.tscn",
	"longspur": "res://combat/longspur_rifle.tscn",
	"rattler": "res://combat/rattler_rifle.tscn",
	"dummy": "res://combat/target_dummy.tscn",
	"pickup": "res://combat/rifle_pickup.tscn",
	"effects": "res://combat/combat_effects.tscn",
	"props": "res://combat/target_range_props.tscn",
}

func _ready() -> void:
	var failures: Array[String] = []
	var loaded := {}
	for key in SCENES:
		var packed := load(SCENES[key]) as PackedScene
		if packed == null:
			failures.append("failed to load %s" % SCENES[key])
		else:
			loaded[key] = packed.instantiate()

	if not failures.is_empty():
		_finish(failures)
		return

	var controller := FakeWeaponController.new()
	add_child(controller)

	var hud: Control = loaded.hud
	add_child(hud)
	hud.bind_controller(controller)
	for path in ["%Crosshair", "%HitMarker", "%ReloadRing", "%WeaponName", "%Ammo", "%State", "%Gait"]:
		if hud.get_node_or_null(path) == null:
			failures.append("HUD missing %s" % path)
	for method in ["bind_controller", "refresh_stats", "set_spread", "set_gait", "begin_reload"]:
		if not hud.has_method(method):
			failures.append("HUD lacks %s" % method)
	hud.set_spread(2.6, "gallop", true)
	hud.begin_reload(2.1)

	var rifle: Node3D = loaded.rifle
	rifle.controller = controller
	add_child(rifle)
	rifle.bind_controller(controller)
	for method in ["equip", "request_fire", "request_reload"]:
		if not rifle.has_method(method):
			failures.append("rifle lacks %s" % method)
	rifle.equip()
	if not rifle.request_fire(42):
		failures.append("rifle did not forward request_fire")
	elif controller.fire_calls.size() != 1:
		failures.append("controller did not receive exactly one fire command")
	else:
		var command: Array = controller.fire_calls[0]
		if absf((command[1] as Vector3).length() - 1.0) > 0.001 or int(command[2]) != 42:
			failures.append("fire evidence direction/tick malformed")

	var dummy: Node3D = loaded.dummy
	dummy.target_id = 7
	dummy.controller = controller
	add_child(dummy)
	dummy.bind_controller(controller)
	controller.hit_confirmed.emit(7, &"head", 28.0)
	if not is_equal_approx(float(dummy.vitality), 72.0):
		failures.append("dummy did not render confirmed native damage")
	if not hud.get_node("%HitMarker").visible:
		failures.append("headshot hit marker did not render")
	if str(hud.get_node("%Ammo").text) != "29 | 120":
		failures.append("ammo HUD did not follow controller signal")

	var pickup: Area3D = loaded.pickup
	add_child(pickup)
	var pickup_events := [0]
	pickup.pickup_requested.connect(func(_id, _mag, _reserve, _node): pickup_events[0] += 1)
	pickup.set_nearby(true)
	pickup.request_pickup()
	if pickup_events[0] != 1 or not pickup.get_node("%Prompt").visible:
		failures.append("pickup prompt/request contract failed")

	var effects: Node3D = loaded.effects
	add_child(effects)
	if effects.show_tracer(Vector3.ZERO, Vector3(0, 0, -4)) == null or effects.show_impact(Vector3(0, 0, -4), Vector3.BACK) == null:
		failures.append("cosmetic tracer/impact methods failed")

	for key in ["longspur", "rattler", "props"]:
		add_child(loaded[key])
	if int(loaded.longspur.weapon_id) != 1 or int(loaded.rattler.weapon_id) != 2:
		failures.append("rifle sidegrade identities are incorrect")

	_check_native_api_if_available(failures)
	await get_tree().process_frame
	_finish(failures)

func _check_native_api_if_available(failures: Array[String]) -> void:
	if not ClassDB.class_exists(&"MountedWeaponController"):
		print("COMBAT_SMOKE_NATIVE_SKIP: build combat GDExtension to exercise native class")
		return
	var native = ClassDB.instantiate(&"MountedWeaponController")
	if native == null:
		failures.append("MountedWeaponController could not instantiate")
		return
	add_child(native)
	for method in ["equip_weapon", "request_fire", "request_reload", "get_weapon_stats", "resolve_local_hit", "resolve_local_miss", "set_rider_context", "advance_to_tick", "begin_saddle_dive", "finish_saddle_dive", "complete_remount"]:
		if not native.has_method(method):
			failures.append("MountedWeaponController lacks %s" % method)
	for signal_name in [&"weapon_changed", &"ammo_changed", &"shot_fired", &"shot_accepted", &"shot_resolved", &"fire_rejected", &"hit_confirmed"]:
		if not native.has_signal(signal_name):
			failures.append("MountedWeaponController lacks %s signal" % signal_name)
	native.queue_free()

func _finish(failures: Array[String]) -> void:
	if failures.is_empty():
		print("SPURFIRE_COMBAT_UI_SMOKE_OK")
		get_tree().quit(0)
	else:
		for failure in failures:
			push_error("COMBAT_SMOKE: " + failure)
		get_tree().quit(1)
