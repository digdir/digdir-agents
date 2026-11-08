# Recording a terminal

`asciinema` records a command in a terminal of its own, and `agg` renders the recording as a GIF.

Set up before recording, so that the recording shows what a user would type: commands on `PATH`, run from a sensible
directory, without full binary paths, custom environment variables or a custom `HOME`, unless that configuration is
what you show. Then record the take and render it:

```sh
env -u NO_COLOR TERM=xterm-256color COLORTERM=truecolor \
  asciinema rec --window-size 120x36 --return --overwrite --command 'bash take.sh' take.cast \
  && mv take.cast terminal.cast                # keep the last good take
agg --font-size 14 terminal.cast terminal.gif
```

The `env` prefix gives the program colour, so leave it out only when what you show is about `NO_COLOR` or `TERM`. A
program in a container needs them passed on, with `podman run -e TERM -e COLORTERM`. `--return` makes `asciinema` fail
when the take does, and `--overwrite` lets it record over the last take.

Have the script print each command it runs, and pause before it and after its output. In a program that takes the
whole terminal, such as a TUI, pause on each state that matters. Record it directly, as in `--command 'agentctl tui'`,
when there is nothing to script. `--idle-time-limit` shortens long waits.

Before handing it over, look at frames from several points, not only the first, with
`agg --select 50% terminal.cast frame.gif`. If picker glyphs are missing, try `agg --font-family 'JetBrains Mono'`.
With the browser feature, `media check terminal.gif` also shows when it holds still long enough to read.
