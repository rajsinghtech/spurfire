extends RefCounted
class_name SpurfireUiPalette

const SUNSET_ZENITH := Color("3a7ca8")
const SUNSET_HORIZON := Color("f7b267")
const SUN_KEY := Color("ffc27a")
const SAND := Color("d9a05b")
const SAND_LIGHT := Color("e2b06a")
const SAND_DARK := Color("c68a49")
const WASH_BED := Color("a06a3f")
const SCRUB := Color("7a8b4f")
const ROCK := Color("b96a4b")
const WOOD := Color("8a5a33")
const WOOD_DARK := Color("5e3d24")
const CREAM := Color("f5e9d0")
const INK := Color("2b1d12")
const BRAND_RED := Color("c44536")
const LOGO_GOLD := Color("f2c879")
const LOGO_ORANGE := Color("f07a3f")
const LOGO_BROWN := Color("8b4c32")
const SLATE := Color("1b222d")
const TECH_TEAL := Color("3fb6c9")

const DISPLAY_SIZE := 42
const TITLE_SIZE := 28
const HEADER_SIZE := 20
const BODY_SIZE := 17
const AUX_SIZE := 15

static func panel_style(opacity := 0.88) -> StyleBoxFlat:
	var style := StyleBoxFlat.new()
	style.bg_color = Color(INK, opacity)
	style.border_width_left = 2
	style.border_width_top = 2
	style.border_width_right = 2
	style.border_width_bottom = 2
	style.border_color = LOGO_BROWN
	style.corner_radius_top_left = 10
	style.corner_radius_top_right = 10
	style.corner_radius_bottom_left = 10
	style.corner_radius_bottom_right = 10
	style.content_margin_left = 14.0
	style.content_margin_right = 14.0
	style.content_margin_top = 12.0
	style.content_margin_bottom = 12.0
	return style
