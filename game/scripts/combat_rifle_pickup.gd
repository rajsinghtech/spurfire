extends Area3D
class_name CombatRiflePickup

signal pickup_requested(weapon_id: StringName, ammo_mag: int, ammo_reserve: int, pickup: CombatRiflePickup)

@export var weapon_id: StringName = &"dustwalker"
@export var display_name := "SF-C30 Dustwalker"
@export var ammo_mag := 30
@export var ammo_reserve := 60
@export var interaction_distance := 3.0
@export var lifetime_after_drop := 30.0
@export var is_dropped := false
@onready var prompt: Label3D = %Prompt

var _age := 0.0
var _origin_y := 0.0
var _nearby := false

func _ready() -> void:
	_origin_y = position.y
	prompt.text = "[E] TAKE %s" % display_name.to_upper()
	prompt.visible = false

func _process(delta: float) -> void:
	_age += delta
	position.y = _origin_y + sin(_age * 2.1) * 0.12
	rotation.y += delta * 0.45
	if is_dropped and _age >= lifetime_after_drop:
		queue_free()

func set_nearby(value: bool) -> void:
	_nearby = value
	prompt.visible = value

func request_pickup() -> void:
	if _nearby:
		pickup_requested.emit(weapon_id, ammo_mag, ammo_reserve, self)

func mark_dropped(mag: int, reserve: int) -> void:
	ammo_mag = mag
	ammo_reserve = reserve
	is_dropped = true
	_age = 0.0
