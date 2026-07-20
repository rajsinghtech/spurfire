extends Control
class_name CombatCrosshair

const PALETTE := preload("res://scripts/ui_palette.gd")

@export var gap := 14.0
@export var tick_length := 9.0
@export var thickness := 3.0
@export var tint := Color("e7edf0")

func _draw() -> void:
	var center := size * 0.5
	_draw_ticks(center, PALETTE.INK, thickness + 2.0)
	_draw_ticks(center, tint, thickness)
	draw_circle(center, 3.5, PALETTE.INK)
	draw_circle(center, 2.0, tint)

func _draw_ticks(center: Vector2, color: Color, width: float) -> void:
	draw_line(center + Vector2(-gap - tick_length, 0), center + Vector2(-gap, 0), color, width, true)
	draw_line(center + Vector2(gap, 0), center + Vector2(gap + tick_length, 0), color, width, true)
	draw_line(center + Vector2(0, -gap - tick_length), center + Vector2(0, -gap), color, width, true)
	draw_line(center + Vector2(0, gap), center + Vector2(0, gap + tick_length), color, width, true)
