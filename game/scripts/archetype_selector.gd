class_name SpurfireArchetypeSelector
extends Control

signal archetype_selected(id: int)

const ARCHETYPE_NAMES := ["Courser", "Warhorse", "Mustang"]
const ACCENTS := [Color("35d5c5"), Color("ef8a45"), Color("ef5aaa")]

@export var horse: Node
@export var horse_path: NodePath
@export_range(0, 2, 1) var selected_id := 0

@onready var attribute_panel = %AttributePanel
@onready var cards: Array[Button] = [%CourserButton, %WarhorseButton, %MustangButton]
@onready var status_label: Label = %Status

func _ready() -> void:
	if horse == null and not horse_path.is_empty():
		horse = get_node_or_null(horse_path)
	for index in cards.size():
		cards[index].pressed.connect(select_archetype.bind(index))
	if horse and horse.has_signal("archetype_changed"):
		horse.connect("archetype_changed", _on_archetype_changed)
		selected_id = clampi(int(horse.get("archetype")), 0, 2)
	_refresh(false)
	cards[selected_id].grab_focus()

func _input(event: InputEvent) -> void:
	if event is InputEventKey and event.physical_keycode == KEY_H and event.is_pressed() and not event.echo:
		visible = not visible
		if visible:
			cards[selected_id].grab_focus()
		get_viewport().set_input_as_handled()
		return
	# Number keys remain fast archetype shortcuts while riding; directional selection is only
	# active while the panel is open so A/D never steals horse controls.
	if not visible:
		if event is InputEventKey and event.is_pressed() and not event.echo:
			match event.physical_keycode:
				KEY_1: select_archetype(0)
				KEY_2: select_archetype(1)
				KEY_3: select_archetype(2)
		return
	if event is InputEventKey and event.physical_keycode == KEY_TAB and event.is_released():
		attribute_panel.set_exact_stats_visible(false)
		return
	if event is InputEventJoypadButton and event.button_index == JOY_BUTTON_Y and event.is_released():
		attribute_panel.set_exact_stats_visible(false)
		return
	if not event.is_pressed() or (event is InputEventKey and event.echo):
		return
	var requested := -1
	if event is InputEventKey:
		match event.physical_keycode:
			KEY_1: requested = 0
			KEY_2: requested = 1
			KEY_3: requested = 2
			KEY_A, KEY_LEFT: requested = wrapi(selected_id - 1, 0, 3)
			KEY_D, KEY_RIGHT: requested = wrapi(selected_id + 1, 0, 3)
			KEY_TAB: attribute_panel.set_exact_stats_visible(true)
	elif event is InputEventJoypadButton:
		match event.button_index:
			JOY_BUTTON_DPAD_LEFT: requested = wrapi(selected_id - 1, 0, 3)
			JOY_BUTTON_DPAD_RIGHT: requested = wrapi(selected_id + 1, 0, 3)
			JOY_BUTTON_Y: attribute_panel.set_exact_stats_visible(true)
	if requested >= 0:
		select_archetype(requested)
		get_viewport().set_input_as_handled()

func set_horse(value: Node) -> void:
	horse = value
	if is_node_ready() and horse:
		if horse.has_signal("archetype_changed") and not horse.is_connected("archetype_changed", _on_archetype_changed):
			horse.connect("archetype_changed", _on_archetype_changed)
		selected_id = clampi(int(horse.get("archetype")), 0, 2)
		_refresh(false)

func select_archetype(id: int) -> void:
	if id < 0 or id > 2:
		push_warning("Archetype selector rejected id %d" % id)
		return
	selected_id = id
	if horse and horse.has_method("set_archetype"):
		horse.call("set_archetype", id)
		# The native controller may reject a moving-horse change. Reflect its
		# authoritative read-only property instead of presenting a false lock-in.
		selected_id = clampi(int(horse.get("archetype")), 0, 2)
	_refresh(selected_id == id)
	archetype_selected.emit(selected_id)

func _on_archetype_changed(_old: int, new: int) -> void:
	selected_id = clampi(new, 0, 2)
	_refresh(false)

func _refresh(show_confirmation: bool) -> void:
	if not is_node_ready():
		return
	var stats: Dictionary = {}
	if horse and horse.has_method("get_archetype_stats"):
		stats = horse.call("get_archetype_stats")
	attribute_panel.set_archetype(selected_id, stats)
	for index in cards.size():
		cards[index].button_pressed = index == selected_id
		cards[index].modulate = Color.WHITE if index == selected_id else Color(0.72, 0.75, 0.80, 1.0)
	cards[selected_id].grab_focus()
	status_label.text = ("READY: " if show_confirmation else "RIDING: ") + ARCHETYPE_NAMES[selected_id].to_upper()
	status_label.add_theme_color_override("font_color", ACCENTS[selected_id])
