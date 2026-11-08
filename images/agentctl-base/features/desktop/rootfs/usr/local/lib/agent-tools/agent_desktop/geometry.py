"""The screenshot budget, and the mapping between display pixels and the pixels of a sent screenshot."""

import math
import re
from collections import namedtuple

# The largest screenshot each harness sends without resizing it, which would break coordinates, as (pixels, longest
# edge). A harness without an entry gets the default.
HARNESS_BUDGETS = {
    # Anthropic's standard tier: https://platform.claude.com/docs/en/build-with-claude/vision
    'claudeCode': (1175000, 1568),
    # OpenAI's `high` detail, Codex's default: https://developers.openai.com/api/docs/guides/images-vision
    'codex': (2457600, 2048),
    'pi': None,
}
# The smallest budget above.
DEFAULT_BUDGET = (1175000, 1568)
# DESKTOP_MAX_WIDTH and DESKTOP_MAX_HEIGHT set a box instead of the table's budget.
DEFAULT_BOX = (1456, 819)
OVERRIDES = ('DESKTOP_SCREENSHOT_PIXELS', 'DESKTOP_SCREENSHOT_MAX_EDGE', 'DESKTOP_MAX_WIDTH', 'DESKTOP_MAX_HEIGHT')


class BudgetError(ValueError):
    pass


# Where a budget comes from is `harness`, `default` without AGENT_HARNESS, or `unknown` for a harness not in the
# table. Its overrides are the environment settings that override the table.
Budget = namedtuple('Budget', 'pixels edge width height source harness overrides')
Geometry = namedtuple('Geometry', 'width height sent_width sent_height')


def budget(environ):
    """The screenshot budget for this harness, with the environment's overrides."""
    harness = environ.get('AGENT_HARNESS', '')
    if not harness:
        source = 'default'
    elif harness in HARNESS_BUDGETS:
        source = 'harness'
    else:
        source = 'unknown'
    pixels, edge = HARNESS_BUDGETS.get(harness) or DEFAULT_BUDGET
    values = {}
    for name in OVERRIDES:
        value = environ.get(name, '')
        if not value:
            continue
        if not re.fullmatch(r'[1-9][0-9]*', value):
            raise BudgetError(f"{name} must be a positive integer without leading zeros, got '{value}'")
        values[name] = int(value)
    if 'DESKTOP_MAX_WIDTH' in values or 'DESKTOP_MAX_HEIGHT' in values:
        return Budget(values.get('DESKTOP_SCREENSHOT_PIXELS'), values.get('DESKTOP_SCREENSHOT_MAX_EDGE'),
                      values.get('DESKTOP_MAX_WIDTH', DEFAULT_BOX[0]), values.get('DESKTOP_MAX_HEIGHT', DEFAULT_BOX[1]),
                      source, harness, tuple(values))
    return Budget(values.get('DESKTOP_SCREENSHOT_PIXELS', pixels), values.get('DESKTOP_SCREENSHOT_MAX_EDGE', edge),
                  None, None, source, harness, tuple(values))


def fit(width, height, budget):
    """The display's size and the size a screenshot of it is sent at: reduced to fit the budget, and rounded down so
    the budget holds. The model addresses the sent size and never does this arithmetic."""
    scale = 1
    if budget.pixels and width * height > budget.pixels:
        scale = math.sqrt(budget.pixels / (width * height))
    if budget.edge and budget.edge / max(width, height) < scale:
        scale = budget.edge / max(width, height)
    if budget.width and budget.width / width < scale:
        scale = budget.width / width
    if budget.height and budget.height / height < scale:
        scale = budget.height / height
    return Geometry(width, height, max(int(width * scale + 1e-9), 1), max(int(height * scale + 1e-9), 1))


def scale(value, source, target):
    """Maps a coordinate between pixel spaces, such as from sent-screenshot pixels to display pixels."""
    return value if source == target else int(value * target / source + 0.5)
