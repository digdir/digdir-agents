"""Puts the `desktop` helper's package, and the `media` command's it uses, as the features install them, on the path of
the tests."""

import os
import sys

features = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..')
for feature in ('desktop', 'browser'):
    sys.path.insert(0, os.path.join(features, feature, 'rootfs', 'usr', 'local', 'lib', 'agent-tools'))
