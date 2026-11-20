#!/usr/bin/env bash
# Prints what decides whether an agent may act on an issue or a pull request: its labels, its assignees and its
# fields in the digdir-agents project. Exits with 2 when the project or the item can't be read, or when gh is too
# old to keep the board current, so that a caller that can't see or show a pause stops instead of carrying on.
set -euo pipefail

if [ "$#" -ne 1 ] || ! [[ "$1" =~ ^[0-9]+$ ]]; then
    echo "usage: work-state.sh <issue or pull request number>" >&2
    exit 64
fi

if ! gh project item-edit --help 2>/dev/null | grep -q -- '--url'; then
    echo "This gh has no 'project item-edit --url', which keeping the board current needs. Update gh. Stop here." >&2
    exit 2
fi

fields='
  state
  labels(first: 50) { nodes { name } }
  assignees(first: 20) { nodes { login } }
  projectItems(first: 20) {
    nodes {
      isArchived
      project { number }
      status: fieldValueByName(name: "Status") { ... on ProjectV2ItemFieldSingleSelectValue { name } }
      wip: fieldValueByName(name: "WIP State") { ... on ProjectV2ItemFieldSingleSelectValue { name } }
    }
  }'

# The project is asked for by itself as well, because GitHub leaves a project the account can't see out of
# projectItems without an error. That would look the same as an item that is not on the board.
query="query(\$number: Int!) {
  organization(login: \"digdir\") { projectV2(number: 74) { title } }
  repository(owner: \"digdir\", name: \"digdir-agents\") {
    issueOrPullRequest(number: \$number) {
      ... on Issue { $fields }
      ... on PullRequest { $fields }
    }
  }
}"

if ! state=$(gh api graphql -F number="$1" -f query="$query" --jq '
  .data.repository.issueOrPullRequest as $item
  | ($item.projectItems.nodes | map(select(.project.number == 74)) | first) as $board
  | "state: \($item.state)",
    "labels: \([$item.labels.nodes[].name] | join(", "))",
    "assignees: \([$item.assignees.nodes[].login] | join(", "))",
    (if $board == null then "project: not on the board"
     else "project: \(if $board.isArchived then "archived" else "on the board" end)",
          "status: \($board.status.name // "")",
          "wip state: \($board.wip.name // "")"
     end)' 2>&1); then
    echo "Can't read the digdir-agents project or #$1, so nothing shows whether work on it is paused. Stop here." >&2
    echo "$state" >&2
    exit 2
fi

echo "$state"
