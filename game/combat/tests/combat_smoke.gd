extends Node

class FakeWeaponController:
	extends Node3D
	signal weapon_changed(weapon_id)
	signal ammo_changed(mag, reserve)
	signal shot_fired(tick, weapon_id)
	signal hit_confirmed(target_id, hit_zone, damage)
	signal fire_rejected(tick, reason)
	signal reload_started(tick, required_ticks)
	signal reload_progressed(tick, progress, active_ticks, required_ticks)
	signal reload_completed(tick, mag, reserve)
	signal reload_rejected(tick, reason)
	var weapon_id := 0
	var fire_calls: Array = []
	var reload_calls := 0
	var mag := 30
	var reserve := 120
	var last_shot_origin := Vector3.ZERO
	var last_shot_direction := Vector3.FORWARD

	func equip_weapon(id) -> bool:
		weapon_id = int(id)
		weapon_changed.emit(weapon_id)
		return true

	func request_fire(origin: Vector3, direction: Vector3, tick: int) -> bool:
		last_shot_origin = origin
		last_shot_direction = direction
		fire_calls.append([origin, direction, tick])
		mag -= 1
		ammo_changed.emit(mag, reserve)
		shot_fired.emit(tick, weapon_id)
		return true

	func request_reload() -> bool:
		reload_calls += 1
		reload_started.emit(42, 126)
		reload_progressed.emit(43, 0.5, 63, 126)
		return true

	func get_weapon_stats() -> Dictionary:
		return {"weapon_id": weapon_id, "display_name": "SF-C30 Dustwalker", "magazine": 30, "reserve": 120, "ammo_mag": mag, "ammo_reserve": reserve, "base_spread_deg": 0.8, "hitscan_clamp_m": 120.0, "is_reloading": false, "reload_progress": 0.0, "reload_ticks": 126}

class FakeEffects:
	extends Node3D
	var tracer_colors: Array[Color] = []

	func show_tracer(_origin: Vector3, _endpoint: Vector3, color: Color = Color.WHITE) -> Node3D:
		tracer_colors.append(color)
		var tracer := Node3D.new()
		add_child(tracer)
		return tracer

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
	for method in ["bind_controller", "refresh_stats", "set_spread", "set_gait", "show_reload_rejection"]:
		if not hud.has_method(method):
			failures.append("HUD lacks %s" % method)
	hud.set_spread(2.6, "gallop", true)
	controller.request_reload()
	if not hud.get_node("%ReloadRing").visible or float(hud.get_node("%ReloadRing").value) != 50.0:
		failures.append("HUD reload ring did not follow native tick progress")
	controller.reload_rejected.emit(44, &"recovering")
	if "RECOVERING" not in str(hud.get_node("%State").text):
		failures.append("HUD did not show reasoned reload rejection")

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
	for key in ["rifle", "longspur", "rattler"]:
		var rig: Node3D = loaded[key]
		var art_muzzle := rig.get_node("WeaponArt/Muzzle") as Marker3D
		if rig.get("muzzle") != art_muzzle:
			failures.append("%s effects are not bound to WeaponArt/Muzzle" % key)
		if rig.get_node("%MuzzleFlash").get_parent() != art_muzzle:
			failures.append("%s muzzle flash is detached from the installed art" % key)

	var tracer_effects := FakeEffects.new()
	var tracer_rider := CharacterBody3D.new()
	var tracer_camera := Camera3D.new()
	add_child(tracer_effects)
	add_child(tracer_rider)
	add_child(tracer_camera)
	var combat_player := CombatPlayer.new()
	combat_player.rider = tracer_rider
	combat_player.controller = controller
	combat_player.rifle = rifle
	combat_player.aim_camera = tracer_camera
	combat_player.effects = tracer_effects
	add_child(combat_player)
	rifle.call("set_weapon", 2)
	combat_player._fire_once(84)
	if tracer_effects.tracer_colors.is_empty() or tracer_effects.tracer_colors[-1] != (rifle.get("tracer_color") as Color):
		failures.append("combat tracer did not use the equipped rifle color")
	if not controller.last_shot_origin.is_equal_approx((rifle.get_node("WeaponArt/Muzzle") as Marker3D).global_position):
		failures.append("fire command did not originate at the installed art muzzle")

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
	for method in ["equip_weapon", "request_fire", "request_reload", "preview_dive_direction", "get_weapon_stats", "resolve_local_hit", "resolve_local_miss", "set_rider_context", "advance_to_tick"]:
		if not native.has_method(method):
			failures.append("MountedWeaponController lacks %s" % method)
	for unsafe_method in ["begin_saddle_dive", "finish_saddle_dive", "complete_remount"]:
		if native.has_method(unsafe_method):
			failures.append("MountedWeaponController exposes forgeable %s transition" % unsafe_method)
	for signal_name in [&"weapon_changed", &"ammo_changed", &"shot_fired", &"shot_accepted", &"shot_resolved", &"fire_rejected", &"reload_started", &"reload_progressed", &"reload_completed", &"reload_rejected", &"hit_confirmed"]:
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
