# Documentation

How to name, place and write the docs in this repository.

- A file whose name is a convention, such as `README.md`, `CHANGELOG.md` or `SKILL.md`, keeps that name. Every other
  document is lower-case kebab-case in a `docs/` directory next to the component it describes, such as
  `agentctl/docs/image-contract.md`, or in the root `docs/` when it spans components.
- Every `docs/` directory has a `README.md` that lists its docs and what each is for, so a reader finds a doc without
  knowing its name. Add a doc to it in the same change that adds the doc.
- Use the terms [GLOSSARY.md](../GLOSSARY.md) defines, the same term for the same thing every time, and add a term
  there when a new concept needs one.
- A README says what the component is, how to install or start it, and links onward.
- `AGENTS.md` holds only what is specific to agents. A rule that applies to people as well goes in the docs everyone
  reads, so that it is said once.
- Give each doc one job: a reference to look things up, steps towards a goal, or an explanation of why something is
  the way it is. None of them describe the implementation. The code is the source for how things work, and
  [architecture.md](architecture.md) keeps a short overview and diagrams.
- Name what acts: "`agentd` refuses the change", not "the change is refused".

## Diagrams

The diagrams in [architecture/](architecture/) are SVGs written as code, usually by an agent, with no drawing tool
or build step. They share one `<style>` block, whose comments say how to use it. Positions are absolute, so look at a
changed diagram rendered and show it in the pull request description. To render one at twice its size, with the window
size from its `viewBox`:

```sh
chromium --headless --hide-scrollbars --force-device-scale-factor=2 --window-size=1600,1040 \
  --screenshot=runtime.png docs/architecture/runtime.svg
```
