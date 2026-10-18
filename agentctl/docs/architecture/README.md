# Architecture diagrams

Diagrams of `agentctl`, `agentd` and the Sandbox crates. [`agentctl/README.md`](../../README.md#architecture) describes
the same architecture in prose.

| Diagram | Shows |
| --- | --- |
| [runtime.svg](runtime.svg) | The processes on the host and in the guest, and what each owns |
| [crates.svg](crates.svg) | The workspace crates, their dependencies and the rules between the layers |
| [network.svg](network.svg) | How guest traffic is mediated and where a secret's real value is used |
| [apply.svg](apply.svg) | What happens from `agentctl apply` to an attached Session |

![What runs where](runtime.svg)

![Two layers, one direction](crates.svg)

![Secrets never enter the Sandbox](network.svg)

![Declared, persisted, then reconciled](apply.svg)

## Editing

The diagrams are hand-written SVG. Edit the source in [`src/`](src/), then build from the repository root:

```sh
make diagrams
```

The build replaces each source's `<!-- @shared -->` marker with the shared stylesheet
([`src/shared.css`](src/shared.css)), the shared arrowheads and shadow ([`src/shared-defs.svg`](src/shared-defs.svg))
and the embedded Inter and JetBrains Mono fonts. It writes the result beside this file, so it renders the same on
GitHub and in every browser. Commit the source and the built SVG together; `make test` fails when they differ.

Reviewers look at the rendered images, so a pull request that changes a diagram shows them in its description. To
render PNGs at twice the size, for example to attach to a pull request:

```sh
node agentctl/docs/architecture/build.mjs --png /tmp/diagrams
```

That needs Chromium on `PATH`, or its path in `CHROMIUM`.

Use the classes in `src/shared.css` rather than inline colours, so the diagrams stay consistent: `zone-host` for the
trusted host, `zone-sandbox` for the untrusted guest, `zone-proc` for a process boundary and `zone-ext` for the
Internet. Keep a diagram in line with the code it shows; when a change moves a boundary, update the diagram in the same
pull request.

The fonts are the Latin subsets of [Inter](https://github.com/rsms/inter) and
[JetBrains Mono](https://github.com/JetBrains/JetBrainsMono), under the SIL Open Font License in [`fonts/`](fonts/).
