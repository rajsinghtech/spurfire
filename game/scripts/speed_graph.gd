extends Control

var samples: PackedFloat32Array = PackedFloat32Array()

func add_sample(speed: float) -> void:
	samples.append(speed)
	if samples.size() > 50:
		samples.remove_at(0)
	queue_redraw()

func _draw() -> void:
	draw_rect(Rect2(Vector2.ZERO, size), Color(0.02, 0.03, 0.04, 0.8), true)
	draw_line(Vector2(0, size.y - 1), Vector2(size.x, size.y - 1), Color(0.4, 0.45, 0.5), 1.0)
	if samples.size() < 2:
		return
	var points := PackedVector2Array()
	for index in samples.size():
		var x := float(index) / 49.0 * size.x
		var y := size.y - clampf(samples[index] / 12.0, 0.0, 1.0) * size.y
		points.append(Vector2(x, y))
	draw_polyline(points, Color(1.0, 0.67, 0.2), 2.0, true)
