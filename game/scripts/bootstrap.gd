extends Control

const MAIN_SCENE := "res://scenes/graybox_course.tscn"

func _ready() -> void:
	if not ClassDB.class_exists(&"HorseController"):
		var message := "HorseController native class unavailable. Build/copy the Spurfire GDExtension library for this platform into game/bin, then restart Godot."
		push_error(message)
		$Panel/Margin/VBox/Error.text = message
		$Panel/Margin/VBox/Paths.text = "Descriptor: res://bin/spurfire.gdextension\nExpected platform libraries are listed there."
		return
	var packed := load(MAIN_SCENE) as PackedScene
	if packed == null:
		push_error("Could not load graybox course: %s" % MAIN_SCENE)
		$Panel/Margin/VBox/Error.text = "Graybox course failed to load. See debugger output."
		return
	# SceneTree is still attaching the bootstrap scene during _ready(). Defer the
	# replacement rather than adding another root child while that operation is active.
	get_tree().change_scene_to_packed.call_deferred(packed)
