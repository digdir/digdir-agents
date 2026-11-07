"""The `desktop` helper, which drives the agent's X11 desktop: observe it, point at it, type into it, record it.

Coordinates are always in the pixels of the screenshot the helper last sent, never in display pixels. When the
display is larger than the screenshot budget, the helper scales them back itself, and `desktop tree` reports its
boxes in the same pixels. If the display has changed size since the last screenshot or tree, commands that take or
report coordinates refuse rather than land somewhere other than what was observed.
"""
