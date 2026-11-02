# dmgbuild settings: icon layout and background for the installer window.
# Icons sit at (180,200) and (480,200); the background arrow is drawn between them.
import os
app = os.environ["APP"]
files = [app]
symlinks = {"Applications": "/Applications"}
icon_locations = {"Copycat.app": (180, 200), "Applications": (480, 200)}
background = os.environ["BG"]
window_rect = ((200, 120), (660, 400))
icon_size = 112
text_size = 13
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
format = "UDZO"
