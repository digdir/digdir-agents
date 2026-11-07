"""Puts the `media` command's package, as the feature installs it, on the path of the tests."""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'rootfs', 'usr', 'local', 'lib',
                                'agent-tools'))
