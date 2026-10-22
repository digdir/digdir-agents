---
theme: default
title: Agents with their own computer — the security view
info: agentctl and the Sandbox SDK for security teams — digdir/digdir-agents
colorSchema: dark
transition: slide-left
mdc: true
fonts:
  sans: Inter
  mono: JetBrains Mono
layout: cover
class: text-left
---

<div class="kicker">digdir/digdir-agents</div>

# Agents with their own computer

<div class="text-2xl muted mt-2">The security view</div>

<div class="accent-bar mt-8"><span /><span /><span /><span /></div>

<div class="absolute bottom-10 left-14 text-sm muted">
Security team · Security champions
</div>

---

<div class="kicker">Threat model</div>

# Assume the agent is hostile

<div class="grid grid-cols-[1fr_1.1fr] gap-10 mt-6">

<div>

<v-clicks>

- An agent runs code **nobody reviewed**: its own, dependencies, scripts from the web
- **Prompt injection** from issues, pages and packages can steer a well-meaning agent
- So we treat it as an attacker with **root inside its VM** and outbound network access
- Without a sandbox, it **runs as you**: your tokens, SSH keys and network

</v-clicks>

</div>

<div class="flex flex-col gap-3" v-click>
  <div class="flex items-center justify-between"><span class="title">What we protect</span><span class="tag blue">assets</span></div>
  <div class="box blue"><div class="title text-sm">The host</div><div class="sub">your machine or the Kubernetes node, its files and its network</div></div>
  <div class="box coral"><div class="title text-sm">Secret values</div><div class="sub">GitHub tokens, model logins, API keys</div></div>
  <div class="box amber"><div class="title text-sm">Other agents and jobs</div><div class="sub">each with its own VM, disk and network</div></div>
  <div class="box green"><div class="title text-sm">Where data goes</div><div class="sub">which hosts the agent may reach, and with which secrets</div></div>
</div>

</div>

---
layout: section
---

<div class="kicker">Part 1</div>

# The landscape

---
class: tight
---

<div class="kicker">Linux kernel · CVEs per year</div>

# The kernel CVE flood

<div class="grid grid-cols-[1.55fr_1fr] gap-6 mt-2 items-start">

<div>
  <img src="/linuxcvetracker-statistics.png" class="rounded-lg border border-[var(--dd-line)]" />
  <div class="text-xs muted mt-1">linuxcvetracker.com, NVD data, 21 Oct 2026. <span class="c-amber">Log scale.</span></div>
</div>

<div v-click>
  <div class="text-sm font-semibold mb-2">The same years on a linear scale</div>
  <div class="flex items-end gap-3 h-44 border-b border-[var(--dd-line)] px-1">
    <div class="flex-1 flex flex-col items-center justify-end h-full">
      <div class="text-xs muted mb-1">263</div>
      <div class="w-full" style="height: 3.3%; background: var(--dd-amber)"></div>
    </div>
    <div class="flex-1 flex flex-col items-center justify-end h-full">
      <div class="text-xs muted mb-1">4,355</div>
      <div class="w-full" style="height: 55.2%; background: var(--dd-amber)"></div>
      <div class="w-full" style="height: 0.4%; background: var(--dd-coral)"></div>
    </div>
    <div class="flex-1 flex flex-col items-center justify-end h-full">
      <div class="text-xs muted mb-1">5,681</div>
      <div class="w-full" style="height: 71.4%; background: var(--dd-amber)"></div>
      <div class="w-full" style="height: 0.6%; background: var(--dd-coral)"></div>
    </div>
    <div class="flex-1 flex flex-col items-center justify-end h-full">
      <div class="text-xs muted mb-1">7,885</div>
      <div class="w-full" style="height: 93.3%; background: var(--dd-amber)"></div>
      <div class="w-full" style="height: 6.7%; background: var(--dd-coral)"></div>
    </div>
  </div>
  <div class="flex gap-3 px-1 text-xs muted mt-1">
    <div class="flex-1 text-center">2023</div><div class="flex-1 text-center">2024</div>
    <div class="flex-1 text-center">2025</div><div class="flex-1 text-center">2026 <span class="c-amber">YTD</span></div>
  </div>
  <div class="text-xs muted mt-1"><span class="c-coral">■</span> Critical: 26 → 47 → <span class="c-coral font-semibold">529</span></div>

  <div class="card mt-3" v-click>
    <h3 class="c-amber">Read it carefully</h3>
    <p>Since February 2024 the kernel is its own CVE authority and assigns a CVE to almost every stable fix. Most of the jump is <strong>counting</strong>, not new bugs. The critical count is the signal.</p>
  </div>
</div>

</div>

---

<div class="kicker">The Register · 22 July 2026</div>

# "Not feasible to prioritize individual kernel changes"

<div class="grid grid-cols-3 gap-5 mt-8">
  <div class="card" v-click>
    <div class="text-4xl font-bold c-amber mb-2">432</div>
    <h3>CVEs in two days</h3>
    <p>Published by the kernel CVE team over one Sunday and Monday in July.</p>
  </div>
  <div class="card" v-click>
    <div class="text-4xl font-bold c-coral mb-2">AI</div>
    <h3>Bug hunting at scale</h3>
    <p>Linus Torvalds, May 2026: the security list has become "almost entirely unmanageable" because of AI-assisted bug hunting.</p>
  </div>
  <div class="card" v-click>
    <div class="text-4xl font-bold c-sky mb-2">↻</div>
    <h3>Patch the fleet, not the CVE</h3>
    <p>Jan Schaumann, Akamai: "This onslaught really shows it's not feasible to attempt to prioritize individual kernel changes."</p>
  </div>
</div>

<div v-click class="mt-10 text-xl">
A container shares the host kernel, so <span class="c-coral font-semibold">every one of these</span> is a potential container escape.<br/>
<span class="muted text-base">Triage cannot keep up. The architecture has to limit how much host kernel untrusted code can reach.</span>
</div>

---
class: tight
---

<div class="kicker">CVE-2026-99999 · PwnEscape</div>

# Hypervisors aren't magic either

<div class="grid grid-cols-[0.9fr_1.2fr] gap-8 mt-2 items-start">

<img src="/vercel-sandbox-bounty.png" class="rounded-lg border border-[var(--dd-line)] max-h-[22rem] mx-auto" />

<div class="text-[0.92rem] leading-relaxed">

<v-clicks>

- Found by Paulos Yibelo (pwn.ai) in **Vercel's $1M Sandbox challenge**: the only Critical, on day three
- AI-assisted: **under $700** of API tokens
- The bug is in the **host kernel's network stack**, not in KVM or Firecracker
- The guest sets **checksum metadata** in the virtio-net header. The host kernel trusts it and rewrites an IPv4 header it already validated
- Host memory read on Vercel's production; host root in a matching KVM + Firecracker lab
- CVSS 9.3. Fixed upstream in `6785011f8b16`, stable kernels on 15 October. **No workaround:** patch the host and reboot

</v-clicks>

</div>
</div>

<div v-click class="mt-3 text-lg text-center">
A VM boundary is only as narrow as <span class="c-coral font-semibold">the host code the guest can reach</span>.
</div>

---
layout: section
---

<div class="kicker">Part 2</div>

# Our boundary

---

<div class="kicker">Isolation</div>

# Where a guest's actions land

<div class="grid grid-cols-3 gap-5 mt-6">

<div class="flex flex-col gap-2">
  <div class="flex items-center justify-between"><span class="title">Container</span><span class="tag coral">shared kernel</span></div>
  <div class="box amber"><div class="title text-sm">Untrusted process</div></div>
  <div class="text-center muted text-xs">every syscall ↓</div>
  <div class="box coral"><div class="title text-sm">Host kernel</div><div class="sub">every syscall and subsystem, so every kernel CVE</div></div>
</div>

<div class="flex flex-col gap-2" v-click>
  <div class="flex items-center justify-between"><span class="title">gVisor</span><span class="tag amber">user-space kernel</span></div>
  <div class="box amber"><div class="title text-sm">Untrusted process</div></div>
  <div class="text-center muted text-xs">syscalls ↓</div>
  <div class="box"><div class="title text-sm">Sentry</div><div class="sub">re-implements Linux in Go</div></div>
  <div class="text-center muted text-xs">reduced set ↓</div>
  <div class="box coral"><div class="title text-sm">Host kernel</div><div class="sub">filtered by seccomp</div></div>
</div>

<div class="flex flex-col gap-2" v-click>
  <div class="flex items-center justify-between"><span class="title">microVM</span><span class="tag green">ours</span></div>
  <div class="box amber"><div class="title text-sm">Untrusted process, root</div></div>
  <div class="text-center muted text-xs">syscalls ↓</div>
  <div class="box"><div class="title text-sm">Guest kernel</div><div class="sub">its own, and untrusted too</div></div>
  <div class="text-center muted text-xs">virtio devices ↓</div>
  <div class="box blue"><div class="title text-sm">VMM in user space</div><div class="sub">libkrun, Rust</div></div>
  <div class="text-center muted text-xs">↓</div>
  <div class="box coral"><div class="title text-sm">Host kernel</div><div class="sub">KVM + what the VMM calls</div></div>
</div>

</div>

<div class="mt-6 text-center text-lg" v-click>
The question is never "VM or container?" but <span class="c-sky font-semibold">which host code does guest-controlled data reach?</span>
</div>

---
class: tight
---

<div class="kicker">Case study · CVE-2026-99999</div>

# The PwnEscape path ends in user space for us

<div class="grid grid-cols-2 gap-8 mt-4">

<div class="flex flex-col gap-2">
  <div class="flex items-center justify-between"><span class="title">TAP / vhost-net</span><span class="tag coral">Firecracker, many KVM hosts</span></div>
  <div class="box amber"><div class="title text-xs">Guest virtio-net</div><div class="sub">frame + <code>virtio_net_hdr</code> with checksum metadata</div></div>
  <div class="text-center muted text-xs">↓ handed to the host kernel</div>
  <div class="box coral"><div class="title text-xs">Host kernel · <code>__virtio_net_hdr_to_skb()</code></div><div class="sub">guest metadata becomes an <code>skb</code></div></div>
  <div class="text-center muted text-xs">↓</div>
  <div class="box coral"><div class="title text-xs">Host IPv4 stack · <code>ip_do_fragment()</code></div><div class="sub">header rewritten after validation 💥</div></div>
</div>

<div class="flex flex-col gap-2" v-click>
  <div class="flex items-center justify-between"><span class="title">Microsandbox</span><span class="tag green">every Agent and runner</span></div>
  <div class="box amber"><div class="title text-xs">Guest virtio-net</div><div class="sub">same frame, same header</div></div>
  <div class="text-center muted text-xs">↓ VMM in user space</div>
  <div class="box blue"><div class="title text-xs">Network backend · header <strong>stripped</strong></div><div class="sub">the checksum metadata is thrown away</div></div>
  <div class="text-center muted text-xs">↓</div>
  <div class="box blue"><div class="title text-xs">smoltcp TCP/IP · authorization · mediator</div><div class="sub">Rust, in user space</div></div>
  <div class="text-center muted text-xs">↓</div>
  <div class="box green"><div class="title text-xs">Host kernel</div><div class="sub">only sees sockets the host opens itself</div></div>
</div>

</div>

<div class="mt-4 text-sm muted text-center" v-click>
We did not predict this bug. The design keeps guest-controlled bytes away from the host kernel, so a whole class of bugs can't be reached.<br/>
AWS: "self-managed KVM hosts using virtio-net/TAP networking can be affected." Microsandbox never selects libkrun's TAP backend.
</div>

---

<div class="kicker">Secrets and egress</div>

# Secrets never enter the Sandbox

```mermaid {scale: 0.55}
sequenceDiagram
  autonumber
  box rgba(229,170,32,0.10) Sandbox · microVM · untrusted
    participant G as gh / git / curl
  end
  box rgba(30,152,245,0.12) Host · trusted
    participant M as Network mediator
    participant S as SecretStore (agentd)
  end
  box rgba(147,164,184,0.08) Internet
    participant GH as api.github.com
    participant E as evil.example
  end
  Note over G: GITHUB_TOKEN=github_pat_AGENT_MEDIATED…
  G->>M: HTTPS request with placeholder
  M->>M: secret.use allowed for this host?
  M->>S: resolve real value
  M->>GH: request with real token
  GH-->>G: response
  G->>M: same placeholder
  M->>E: not an allowed host, so only the inert placeholder
```

<div class="text-sm muted mt-1 text-center">
Every layer ends on the host · <code>sandbox-authorization</code> for every check · Per-secret <code>allowedHosts</code> · Fails closed
</div>

---

<div class="kicker">agentctl/CHANGELOG.md · Security</div>

# Found and fixed

<div class="grid grid-cols-2 gap-5 mt-6">
  <div class="card" v-click>
    <h3 class="c-coral">Bypass after an agentd restart</h3>
    <p>Changing a running Agent's resources after a restart gave it network access around authorization and secret mediation.</p>
  </div>
  <div class="card" v-click>
    <h3 class="c-coral">Tunnelling through an allowed host</h3>
    <p>Non-HTTP traffic inside an HTTPS connection to an allowed host bypassed authorization. WebSockets still work.</p>
  </div>
  <div class="card" v-click>
    <h3 class="c-amber"><code>.env</code> in bind mounts</h3>
    <p>Applying an Agent rejects bind mounts that contain <code>.env</code> files, case-insensitively and regardless of ignore rules.</p>
  </div>
  <div class="card" v-click>
    <h3 class="c-amber">TLS dependencies</h3>
    <p>rustls advisories assessed and upgraded. Certificates chaining through the distrusted emSign intermediates are rejected.</p>
  </div>
</div>

<div class="mt-8 text-lg" v-click>
The boundary is treated as a <span class="c-sky font-semibold">security boundary</span>: fixes are released, and the changelog's Security section names them.
<div class="text-sm muted mt-2">CI runs <code>cargo deny</code> for advisories and dependency sources on every change.</div>
</div>

---
class: tight
---

<div class="kicker">Honest limits · host side</div>

# Host attack surface that remains

<div class="grid grid-cols-[1.35fr_1fr] gap-6 mt-2">

<div class="flex flex-col gap-2">
  <div class="box"><div class="flex items-center justify-between"><span class="title text-sm">KVM</span><span class="tag">host kernel</span></div><div class="sub">Shared by every KVM product. Patched with the host.</div></div>
  <div class="box"><div class="flex items-center justify-between"><span class="title text-sm">VMM devices</span><span class="tag">libkrun · Rust</span></div><div class="sub">virtio-fs for the root filesystem and bind mounts, block, vsock, console</div></div>
  <div class="box"><div class="flex items-center justify-between"><span class="title text-sm">Host-side parsers</span><span class="tag">Rust</span></div><div class="sub">smoltcp, DNS, TLS termination with rustls, HTTP for authorization and mediation</div></div>
  <div class="box coral" v-click><div class="flex items-center justify-between"><span class="title text-sm">The VMM process isn't confined</span><span class="tag coral">gap</span></div><div class="sub">It runs as your user with no seccomp filter, Landlock or namespaces. Firecracker ships a jailer and seccomp filters. A VMM bug means code execution as you, on your machine.</div></div>
  <div class="box green" v-click><div class="flex items-center justify-between"><span class="title text-sm">Kubernetes</span><span class="tag green">second layer</span></div><div class="sub">The VMM runs in a non-privileged pod that only gets <code>/dev/kvm</code> from a device plugin.</div></div>
</div>

<div class="flex flex-col gap-4" v-click>
  <div class="big-quote" style="font-size: 1.45rem">
  "Design for the assumption that a guest will eventually escape its <span class="c-coral">first boundary</span>."
  </div>
  <div class="text-xs muted">Cloud Security Alliance, research note on PwnEscape</div>
  <div class="card mt-2"><h3>Next layer to add</h3><p>Confine the VMM: a seccomp allowlist, Landlock limited to the Agent's own state and mounts, and dropped capabilities. It is cheap compared to a host compromise.</p></div>
</div>

</div>

---
class: tight
---

<div class="kicker">Honest limits · inside the boundary</div>

# What the Sandbox doesn't protect against

<div class="grid grid-cols-2 gap-8 mt-4">

<div class="flex flex-col gap-3">
  <div class="flex items-center justify-between"><span class="title">Protected</span><span class="tag green">by design</span></div>
  <div class="box green"><div class="title text-sm">Your machine</div><div class="sub">the agent has root only inside its own VM</div></div>
  <div class="box green"><div class="title text-sm">Secret values</div><div class="sub">never in the VM, only sent to their allowed hosts</div></div>
  <div class="box green"><div class="title text-sm">Other agents</div><div class="sub">separate VMs, disks and networks</div></div>
  <div class="box green"><div class="title text-sm">The host network</div><div class="sub">only the Platform API is reachable</div></div>
</div>

<div class="flex flex-col gap-3" v-click>
  <div class="flex items-center justify-between"><span class="title">Not protected</span><span class="tag coral">needs other controls</span></div>
  <div class="box coral"><div class="title text-sm">Misuse of a token on its own hosts</div><div class="sub">anything the PAT allows on github.com, including a public gist</div></div>
  <div class="box coral"><div class="title text-sm">Data leaving without secrets</div><div class="sub">with <code>allow: all</code>, code can be posted to any host</div></div>
  <div class="box coral"><div class="title text-sm">Prompt injection</div><div class="sub">issues, web pages and dependencies can steer the agent</div></div>
  <div class="box coral"><div class="title text-sm">Bad code</div><div class="sub">review is still the gate, not the Sandbox</div></div>
  <div class="box coral"><div class="title text-sm">Sessions in one Sandbox</div><div class="sub">same user and tmux server, so one trust boundary</div></div>
</div>

</div>

<div class="mt-4 text-sm muted text-center" v-click>
Narrow tokens, tighter <code>allow</code> lists and review shrink the gap. The policy engine is where stricter rules can go.
</div>

---

<div class="kicker">altinn-studio · src/ci/github-runner</div>

# Same boundary, other workloads

```mermaid {scale: 0.75}
flowchart LR
  GH["GitHub Actions<br/>queued job"] -->|scale on queue| K["KEDA ScaledJob<br/><small>0 → 64</small>"]
  K --> POD
  subgraph POD["Kubernetes Job (one per run) · non-privileged"]
    direction LR
    I["init: prepared image<br/><small>no registry creds at runtime</small>"] --> CO["coordinator<br/><small>Sandbox SDK</small>"]
    CO --> VM["microVM<br/><small>runner + Docker</small>"]
  end
  CO -->|GitHub App| GH
  VM -->|short-lived registration token| GH
```

<div class="grid grid-cols-3 gap-4 mt-4">
  <div class="card"><h3>Untrusted PR code</h3><p>CI runs whatever a pull request contains. One VM per job, with a private root, workspace and Docker data.</p></div>
  <div class="card"><h3>Credentials stay out</h3><p>The GitHub App key stays in the coordinator. The guest gets a one-time token and drops it before the runner starts.</p></div>
  <div class="card"><h3>Docker without privilege</h3><p>Containers run inside the VM instead of in a privileged pod or on a mounted Docker socket.</p></div>
</div>

<div class="mt-4 text-sm muted text-center">
The Sandbox SDK has no agent concepts. Any workload that runs untrusted code can use the same boundary.
</div>

---

<div class="kicker">Future · not decided</div>

# What's next

<div class="grid grid-cols-3 gap-5 mt-8">
  <div class="card" v-click>
    <div class="text-3xl mb-3">🛡️</div>
    <h3 class="c-sky">Company AI gateway</h3>
    <p>Send model and MCP traffic through Digdir's gateway: one place for logging, policy and cost. MCP servers are proxied and vetted centrally instead of per agent. Open models for security work on code that must not leave Digdir.</p>
  </div>
  <div class="card" v-click>
    <div class="text-3xl mb-3">🧩</div>
    <h3 class="c-amber">Plugin architecture</h3>
    <p>New capabilities as host plugins behind a Platform API. They run on the trusted side with their own authorization, so the Sandbox gets a capability without getting the credentials behind it.</p>
  </div>
  <div class="card" v-click>
    <div class="text-3xl mb-3">🏭</div>
    <h3 class="c-green">Cloud agents</h3>
    <p>Software factories: agents started from an issue or a pipeline and run on Kubernetes, like the GitHub runners. The same boundary, at a scale where identity, review and tenancy become the hard questions.</p>
  </div>
</div>

---
layout: end
---

<div class="kicker">Thanks</div>

# Questions?

<div class="accent-bar mx-auto mt-6"><span /><span /><span /><span /></div>

<div class="mt-10 text-sm muted leading-7">
github.com/digdir/digdir-agents · <code>agentctl/README.md</code> · <code>agentctl/CHANGELOG.md</code><br/>
github.com/digdir/microsandbox · github.com/digdir/libkrunfw<br/>
Sources: theregister.com · pwn.ai/blog/pwnescape · linuxcvetracker.com · labs.cloudsecurityalliance.org
</div>
