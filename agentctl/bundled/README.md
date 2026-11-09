# Bundled files

Files that `agentctl` and `agentd` embed in their build, such as the skill every agent gets and the
[definitions of the agents agentd runs for itself](definitions/). Keep every embedded file in this folder: CI, the
agentctl image's build and its path filters name only this folder, so a file embedded from anywhere else would leave
them out of date.

Markdown here is code, not documentation, so CI runs the Rust checks when it changes.
