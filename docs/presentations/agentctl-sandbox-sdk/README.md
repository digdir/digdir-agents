# Agents with their own computer

A [Slidev](https://sli.dev) presentation of `agentctl` and the Sandbox SDK: the architecture, what an Agent definition
is, the productivity it brings, the other workloads on the same SDK and onboarding from zero to a running prompt. It
is written for Digdir's DevOps teams, with Altinn Studio as the example.

## Run it

```sh
npm ci
npm run dev
```

Open <http://localhost:3030>. `f` toggles fullscreen, `o` shows an overview and the arrow keys step through each
slide's clicks. The presenter view is at <http://localhost:3030/presenter/>.

The dev server listens on `127.0.0.1` only. From inside an Agent, forward the port to your machine:

```sh
agentctl port-forward <agent> 3030:3030
```

`npm run build` writes a static site to `dist/`.

## Files

- `slides.md`: the slides
- `style.css`: the Digdir colours and the shared card and box styles
- `setup/mermaid.ts`: the Mermaid theme
- `global-bottom.vue`: the footer
