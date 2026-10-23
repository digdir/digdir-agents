# Agents with their own computer: the security view

A shorter [Slidev](https://sli.dev) presentation of `agentctl` and the Sandbox SDK for Digdir's security team and
security champions. It sets the microVM boundary against the rise in Linux kernel CVEs and the PwnEscape guest-to-host
escape (CVE-2026-99999), then covers secret mediation, recent security fixes, the remaining host attack surface and
what the Sandbox does not protect against, the sandboxed GitHub runners and what may come next. The general
presentation is in [`../agentctl-sandbox-sdk`](../agentctl-sandbox-sdk/).

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
- `public/`: screenshots of the [Linux CVE Tracker statistics](https://linuxcvetracker.com/cve-statistics/) and of
  the Vercel Sandbox bounty for PwnEscape

## Sources

- [Linux kernel team publishes 432 CVEs in two days](https://www.theregister.com/security/2026/07/22/linux-kernel-team-publishes-432-cves-in-two-days/5276497),
  The Register, 22 July 2026
- [PwnEscape](https://pwn.ai/blog/pwnescape), pwn.ai
- [Research note on PwnEscape](https://labs.cloudsecurityalliance.org/research/csa-research-note-pwnescape-linux-kernel-vm-escape-agent-san/),
  Cloud Security Alliance
- [Linux kernel CVE statistics](https://linuxcvetracker.com/cve-statistics/), Linux CVE Tracker
