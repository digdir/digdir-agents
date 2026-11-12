# Agent variants

An agent variant, `agent.<variant>.yaml` with `kind: AgentVariant`, holds only what differs from the manifest it
`extends`, which sits beside it. agentctl applies it to that manifest as Kustomize applies a
[strategic merge patch](https://github.com/kubernetes/community/blob/main/contributors/devel/sig-api-machinery/strategic-merge-patch.md)
in an overlay, so a variant reads as a Kustomize patch does, except where this page says otherwise.

## Merging

- A mapping merges field by field, and a value replaces the inherited one. `null` removes the inherited field, and
  changes nothing where the base sets none.
- A list of entries merges entry by entry, on the field that names an entry, such as a harness's `type`, an
  environment variable's `name` or a mount's `target`. An entry with an inherited key changes that entry, and one with
  a new key is added. A manifest can't repeat a key, and a variant can't name one twice.
- A list of strings, such as `network.deny`, replaces the inherited one, since a string has no key.

## Directives

```yaml
spec:
  access:
    - type: vnc
      $patch: delete    # removes the inherited vnc entry
  skills:
    - $patch: replace   # the variant's skills replace the inherited ones
    - source: skills/review
  home:
    $patch: delete      # removes the inherited home, as null does
```

- `$patch: delete` on an entry removes the inherited entry with its key. A base that no longer has the entry is no
  error, so the variant keeps working when its base drops it.
- `$patch: replace` in an entry of its own makes the variant's entries replace the inherited list, in the variant's
  order. On a mapping, it replaces the inherited mapping.
- `$patch: delete` on a mapping removes it, as `null` does.

A variant takes only the directives that Kustomize and Kubernetes apply the same way, and refuses the others rather
than pick one tool's reading. It refuses an empty list of entries too: Kustomize leaves the inherited entries as they
are, which is rarely what `[]` means. Write `null` to remove them.

## Where a variant differs from Kustomize

- **Order.** A variant adds to its base, so its new entries follow the inherited ones, and an entry it changes or
  restates keeps its place. Kustomize puts a patch's entries first. Where order means something, the base's stays,
  such as which bind mount's folder names the agents `agentctl run` makes. `$patch: replace` gives any other order.
- **A different `type`.** A mapping or entry with another `type`, such as a mount that changes from `tmpfs` to `bind`,
  replaces the inherited one whole, since the old type's fields don't belong to the new one. Kustomize merges the two.
