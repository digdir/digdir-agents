# Browser captures

Use a named session per task. Keep the viewport consistent when comparing revisions. Run capture commands from
the artifact directory.

```sh
export PLAYWRIGHT_CLI_SESSION=<task>
playwright-cli open --browser chromium http://localhost:8080/
playwright-cli resize 1280 720
playwright-cli snapshot
# Navigate using the snapshot's element refs.
playwright-cli screenshot --filename=result.png
```

For an interaction, record the scenario with pauses long enough to follow each action and read its result:

```sh
playwright-cli video-start browser.webm
playwright-cli run-code --filename=scenario.js
playwright-cli video-stop
media check browser.webm
playwright-cli close
```

`scenario.js` supplies an async Playwright function taking `page`. Use `playwright-cli --help` for commands or the
`playwright-cli` skill for more involved automation. Standalone Playwright scripts are also an option: run them with
`NODE_PATH="$(npm root -g)" node script.js`, and in an ES module load Playwright with
`createRequire(import.meta.url)('playwright')`, since `import` ignores `NODE_PATH`.

Inspect the screenshot or the contact sheet produced by `media check` before attaching.
