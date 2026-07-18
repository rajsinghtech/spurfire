extends Area3D

@export var gameplay_coordinator: Node

func _ready() -> void:
	body_entered.connect(_on_body_entered)
	if gameplay_coordinator == null:
		gameplay_coordinator = get_node_or_null("../M2Gameplay")

func _on_body_entered(_body: Node) -> void:
	# The shared coordinator allocates the next absolute tick. Kill-plane
	# callbacks never invent current_tick + 1 and therefore cannot replay the
	# coordinator's next movement/combat/network tick.
	if gameplay_coordinator and gameplay_coordinator.has_method("request_course_reset"):
		gameplay_coordinator.call("request_course_reset")
