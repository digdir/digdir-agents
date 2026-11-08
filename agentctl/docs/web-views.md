# Declare the web UIs an agent serves

An agent's image may serve web UIs, such as T3 Code or a desktop in the browser. Declaring them in the manifest lets
clients list an agent's views without knowing its image. This guide declares them.
[agent-resource.md](agent-resource.md#specaccess) has the rules for each field.

## Declare a view

Add a `web` entry to `spec.access` for each UI, with a name and the port it listens on in the sandbox:

```yaml
spec:
  access:
    - type: ssh
    - type: web
      name: t3code
      port: 3773
    - type: web
      name: docs
      port: 8080
      path: /docs/
```

`path` is where the view opens, and defaults to `/`. Apply the manifest, then check what clients see:

```sh
agentctl describe agent/NAME
agentctl get agent/NAME -o json
```

`describe` lists access as `ssh,web/t3code:3773,web/docs:8080`, and the JSON has each view under `spec.access`.

The manifest is read on the host, so only the manifest declares views. A process in the sandbox can't add one.

## Choose a name

Clients and commands refer to a view by its name, so pick a short name for what it serves, such as `t3code`, `docs`
or `desktop`. It is 1 to 63 lowercase letters, digits and `-`, and each view of an agent needs its own.

## Serve a desktop in the browser

`type: vnc` is the direct VNC connection. A desktop in the browser, such as noVNC, is a `web` view on the port the
image serves it on:

```yaml
    - type: web
      name: desktop
      port: 6080
      path: /vnc.html
```

## Change or remove a view in a variant

A variant matches views by name. See [Change an inherited list in a variant](variant-lists.md).

## What's not there yet

`agentctl` doesn't open views yet. A view's forward will listen on the host's loopback interface, which only the host
reaches, so a view needs no sign-in of the platform's own. Sign-in through a reverse proxy can come later as a field
on the view.
