# agentctl status --json

`agentctl status --json` prints one JSON object: agentd, the harness logins, the stored GitHub accounts, the agent
the current directory resolves to, a count of every agent, and what to run next. It starts agentd if it is not running.

The object is a contract. Keys are `snake_case`, enumerations are the strings listed below, and anything absent is
`null`, never left out. Later releases add keys without changing these; a change to an existing key raises `version`.

```json
{
  "version": 1,
  "agentd": {"version": "0.9.0", "home": "/Users/person/.agent"},
  "harnesses": [
    {"harness": "claudeCode", "auth": "mediated", "login_stored": true},
    {"harness": "codex", "auth": "mediated", "login_stored": false},
    {"harness": "pi", "auth": "none", "login_stored": null}
  ],
  "github": [
    {"name": "digdir", "account": "digdir-bot", "kind": "fineGrained", "expires_at": "2027-01-13T10:00:00Z", "expired": false}
  ],
  "directory": {
    "path": "/Users/person/code/app",
    "agent": {"name": "dev", "state": "failed", "reason": "the image does not exist", "sessions": 0, "sessions_waiting": 0},
    "problem": null
  },
  "agents": {"total": 2, "running": 1, "starting": 0, "failing": 1, "stopped": 0, "sessions_waiting": 0},
  "next": [{"command": "agentctl describe agent/dev", "purpose": "See the details of what dev needs"}]
}
```

- `agentd`: the agentd that answered, its build `version` and its `home` directory.
- `harnesses`: every harness agentctl supports, by its `harness` name (`claudeCode`, `codex` or `pi`), with its
  `auth` mode (`mediated` or `none`) and whether its login is stored, `login_stored`, which is `null` for a harness
  with no platform login.
- `github`: every stored GitHub token, `[]` when there is none, never the token itself: the `name` it is stored
  under, which manifests ask for, the `account` it acts as, its `kind` (`fineGrained`, `classic`, `oauth` or
  `other`), when it `expires_at` as GitHub said at login, `null` when GitHub stated no expiry, and whether it has
  `expired`. A browser sign-in is an `oauth` token. An agent whose
  manifest asks for a name without an unexpired token runs without GitHub access until `agentctl github login` stores
  one.
- `directory`: the current `path`, and the `agent` it resolves to as `attach` would, or `null`. An agent is found from
  its manifest's folder, a folder it bind-mounts, or anywhere in the Git checkout its manifest is in. `problem` says
  why the directory cannot resolve to one agent, such as several agents applied from it.
- `directory.agent`: its `name`; its `state` as `agentctl tui` shows it, one of `ready`, `pending`, `starting`,
  `provisioning`, `retrying`, `failed`, `unresponsive`, `stopping`, `restarting`, `stopped` or `terminating`; the `reason` when
  it is failing; its unarchived `sessions`, and how many of those are `sessions_waiting` for the person.
- `agents`: the `total` not being deleted, how many are `running`, `starting`, `failing` and `stopped`, and how many
  sessions are `sessions_waiting` for the person.
- `next`: at most two steps that would change what status shows, the most pressing first: the `command` to run
  and its `purpose`. Empty when nothing needs doing.
