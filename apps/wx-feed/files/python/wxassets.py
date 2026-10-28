"""wxassets: where the renderer finds pictures that it doesn't draw itself.

Only our own files are bundled (fonts, app icons). Logos and footage that belong to
someone else are never committed, so each widget draws a placeholder of its own when
the file isn't there. To use your own copies on your own server, put them in a
directory and name it in NEOCHUMBY_WX_ASSETS (several, separated by ':'); those are
searched first. The names the widgets look for:

  logo.png             the aurora screensaver's logo (RGBA, about 220 x 64)
  octopus.png          the small mark on Home, Settings and the leave dialog (RGBA)
  weather-badge.png    the weather pages' badge, top left (RGBA)
  raysclip.png         the aurora's light-ray clip (raysclip.py makes it from a video)
  abc-logo.png, abc-swirls.png   the news header's logo and swirls (news.py make_assets)
"""
import os

HERE = os.path.dirname(os.path.abspath(__file__))
BUNDLED = os.path.join(HERE, "assets")


def dirs():
    extra = [d for d in os.environ.get("NEOCHUMBY_WX_ASSETS", "").split(os.pathsep) if d]
    return extra + [BUNDLED]


def find(name):
    """The path of asset `name`, or None if no asset directory has it."""
    if os.sep in name or name.startswith("."):
        return None
    for d in dirs():
        p = os.path.join(d, name)
        if os.path.isfile(p):
            return p
    return None
