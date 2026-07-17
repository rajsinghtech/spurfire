extends Control
class_name CombatCrosshair

@export var gap := 14.0
@export var tick_length := 9.0
@export var thickness := 3.0
@export var tint := Color("e7edf0")

func _draw() -> void:
	var center := size * 0.5
	draw_line(center + Vector2(-gap - tick_length, 0), center + Vector2(-gap, 0), tint, thickness, true)
	draw_line(center + Vector2(gap, 0), center + Vector2(gap + tick_length, 0), tint, thickness, true)
	draw_line(center + Vector2(0, -gap - tick_length), center + Vector2(0, -gap), tint, thickness, true)
	draw_line(center + Vector2(0, gap), center + Vector2(0, gap + tick_length), tint, thickness, true)
	draw_circle(center, 2.0, tint)
