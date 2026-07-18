extends Area3D

func _ready() -> void:
	body_entered.connect(_on_body_entered)

func _on_body_entered(body: Node) -> void:
	if body.has_method("reset_rider"):
		body.call("reset_rider", int(body.get("current_tick")) + 1)
	elif body.has_method("reset_horse"):
		var rider := body.get_parent().get_node_or_null("Rider")
		if rider and rider.has_method("reset_rider"):
			rider.call("reset_rider", int(rider.get("current_tick")) + 1)
		else:
			body.reset_horse()
