//! The `OpenCode` plugin that reports activity to the platform and guards the Session's model.
//!
//! `OpenCode` has no hook commands; it loads TypeScript plugins in its server. The plugin follows
//! the server's event stream, which carries the same signals the other harnesses' hooks do, and
//! reports them with the code shared with the hook scripts. It also fails a model request when the
//! model or variant the Session selected is one the Agent's `OpenCode` configuration lacks, which
//! `OpenCode` would otherwise replace silently.

use crate::harness::hook_script;

const TEMPLATE: &str = include_str!("activity_plugin.ts");

/// Renders the activity plugin, which the launch's configuration layer loads.
///
/// # Errors
///
/// Returns an error when the endpoint locations cannot be encoded.
pub(super) fn script() -> Result<String, serde_json::Error> {
    Ok(TEMPLATE.replace("__REPORTER__", &hook_script::reporter()?))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::harness::hook_script::test_support::{Run, pairs, summary};

    const NATIVE: &str = "ses_eb1aa5fc9ffeOm3nIXACjLOcWG";

    #[test]
    fn the_rendered_plugin_has_no_placeholders_left() {
        let script = super::script().expect("script");
        assert!(!script.contains("__"), "{script}");
        assert!(script.contains("/etc/agent-platform-api-url"));
        assert!(script.contains("/v1/session/hooks"));
    }

    /// Sets the rendered plugin up under Node with a fake `OpenCode` `ctx` and runs `driver`, which
    /// may `await emit(type, data)` an event, `await context()` a model request and `print` a
    /// value, and `agents(byId)` replays the agent transforms. `options` are the plugin's options and `models` what `ctx.model.list()` returns.
    fn run(options: &Value, models: &Value, driver: &str, environment: &[(&str, &str)]) -> Run {
        let driver = format!(
            r#"import plugin from "./index.ts";
// Events wait in a queue; `emit` resolves once the plugin has handled its event and asks for the next.
const queue = [];
let wake = () => {{}};
const emit = (type, data) => new Promise((handled) => {{ queue.push({{ event: {{ type, data }}, handled }}); wake(); }});
async function* subscribe() {{
  for (;;) {{
    if (queue.length === 0) {{ await new Promise((resolve) => {{ wake = resolve; }}); continue; }}
    const {{ event, handled }} = queue.shift();
    yield event;
    handled();
  }}
}}
const hooks = {{}};
let listed = 0;
const transforms = [];
// Replays the registered agent transforms onto `agents`, as OpenCode does, and returns them.
const agents = (agents) => {{
  const editor = {{
    list: () => Object.values(agents),
    update: (id, update) => update(agents[id]),
  }};
  for (const transform of transforms) transform(editor);
  return agents;
}};
const ctx = {{
  app: {{ name: "opencode", version: "2.0.32", channel: "latest" }},
  options: {options},
  event: {{ subscribe }},
  session: {{ hook: async (name, callback) => {{ hooks[name] = callback; return {{ dispose: async () => {{}} }}; }} }},
  model: {{ list: async () => {{ listed += 1; return {{ location: {{}}, data: {models} }}; }} }},
  agent: {{ transform: async (callback) => {{ transforms.push(callback); return {{ dispose: async () => {{}} }}; }} }},
}};
const cleanup = await plugin.setup(ctx);
const print = (value) => console.log(JSON.stringify(value));
const context = async () => {{
  try {{ await hooks.context({{ sessionID: "{NATIVE}" }}); return "allowed"; }} catch (error) {{ return error.message; }}
}};
{driver}
cleanup();
"#
        );
        let script = super::script().expect("script");
        let home = tempfile::TempDir::new().expect("home");
        let home_path = home.path().to_str().expect("UTF-8 home").to_owned();
        let mut environment = environment.to_vec();
        if !environment.iter().any(|(name, _)| *name == "HOME") {
            environment.push(("HOME", &home_path));
        }
        crate::harness::hook_script::test_support::run("index.ts", &script, &driver, &environment)
    }

    fn reports(driver: &str) -> Vec<Value> {
        run(&json!({}), &json!([]), driver, &[]).reports
    }

    /// A model as `ctx.model.list()` describes it, with only the fields the plugin reads.
    fn model(provider: &str, id: &str, variants: &[&str]) -> Value {
        json!({
            "providerID": provider,
            "id": id,
            "variants": variants.iter().map(|variant| json!({"id": variant})).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn a_new_conversation_reports_its_start_and_turns() {
        let reports = reports(&format!(
            r#"
await emit("session.created", {{ sessionID: "{NATIVE}", slug: "silent-otter" }});
await emit("session.execution.started", {{ sessionID: "{NATIVE}" }});
await emit("session.step.started", {{ sessionID: "{NATIVE}" }});
await emit("session.execution.succeeded", {{ sessionID: "{NATIVE}" }});
await emit("session.execution.started", {{ sessionID: "{NATIVE}" }});
await emit("session.execution.failed", {{ sessionID: "{NATIVE}", error: {{ type: "provider.auth", message: "no" }} }});
await emit("session.execution.started", {{ sessionID: "{NATIVE}" }});
await emit("session.execution.interrupted", {{ sessionID: "{NATIVE}", reason: "user" }});
"#
        ));

        assert_eq!(
            summary(&reports),
            pairs(&[
                ("sessionStart", "startup"),
                ("turnStarted", "user"),
                ("turnCompleted", "succeeded"),
                ("turnStarted", "user"),
                ("turnCompleted", "failed"),
                ("turnStarted", "user"),
                ("turnCompleted", "interrupted"),
            ])
        );
        assert_eq!(reports[0]["nativeSessionId"], NATIVE);
        assert!(
            reports
                .iter()
                .all(|report| report["eventId"].is_string() && report["sessionId"] == json!("session-1"))
        );
    }

    #[test]
    fn a_resumed_conversation_is_reported_when_its_first_execution_starts() {
        // A resumed conversation, or one switched to in the TUI, has no `session.created`.
        let reports = reports(&format!(
            r#"
await emit("session.execution.started", {{ sessionID: "{NATIVE}" }});
await emit("session.execution.succeeded", {{ sessionID: "{NATIVE}" }});
await emit("session.execution.started", {{ sessionID: "{NATIVE}" }});
await emit("session.execution.started", {{ sessionID: "ses_other" }});
"#
        ));

        assert_eq!(
            summary(&reports),
            pairs(&[
                ("sessionStart", "resume"),
                ("turnStarted", "user"),
                ("turnCompleted", "succeeded"),
                ("turnStarted", "user"),
                ("sessionStart", "resume"),
                ("turnStarted", "user"),
            ])
        );
        assert_eq!(reports[0]["nativeSessionId"], NATIVE);
        assert_eq!(reports[4]["nativeSessionId"], "ses_other");
    }

    #[test]
    fn an_end_without_a_reported_start_completes_nothing() {
        let reports = reports(&format!(
            r#"
await emit("session.execution.succeeded", {{ sessionID: "{NATIVE}" }});
await emit("session.execution.started", {{ sessionID: "{NATIVE}" }});
await emit("session.execution.succeeded", {{ sessionID: "{NATIVE}" }});
await emit("session.execution.succeeded", {{ sessionID: "{NATIVE}" }});
"#
        ));

        assert_eq!(
            summary(&reports),
            pairs(&[
                ("sessionStart", "resume"),
                ("turnStarted", "user"),
                ("turnCompleted", "succeeded"),
            ])
        );
    }

    #[test]
    fn a_subagent_session_neither_starts_nor_ends_the_turn_but_its_questions_block_it() {
        let reports = reports(&format!(
            r#"
await emit("session.created", {{ sessionID: "{NATIVE}" }});
await emit("session.execution.started", {{ sessionID: "{NATIVE}" }});
await emit("session.created", {{ sessionID: "ses_child", parentID: "{NATIVE}" }});
await emit("session.execution.started", {{ sessionID: "ses_child" }});
await emit("form.created", {{ form: {{ id: "frm_1", sessionID: "ses_child" }} }});
await emit("form.replied", {{ id: "frm_1", sessionID: "ses_child" }});
await emit("session.execution.succeeded", {{ sessionID: "ses_child" }});
await emit("session.execution.succeeded", {{ sessionID: "{NATIVE}" }});
"#
        ));

        assert_eq!(
            summary(&reports),
            pairs(&[
                ("sessionStart", "startup"),
                ("turnStarted", "user"),
                ("waitingForInput", "form"),
                ("turnStarted", "form"),
                ("turnCompleted", "succeeded"),
            ])
        );
    }

    #[test]
    fn a_question_or_permission_blocks_the_turn_and_resumes_it_only_inside_a_run() {
        let reports = reports(&format!(
            r#"
await emit("permission.asked", {{ id: "per_1", sessionID: "{NATIVE}" }});
await emit("permission.replied", {{ id: "per_1", sessionID: "{NATIVE}" }});
await emit("session.execution.started", {{ sessionID: "{NATIVE}" }});
await emit("permission.asked", {{ id: "per_2", sessionID: "{NATIVE}" }});
await emit("permission.replied", {{ id: "per_2", sessionID: "{NATIVE}" }});
await emit("form.created", {{ form: {{ id: "frm_1", sessionID: "{NATIVE}" }} }});
await emit("form.cancelled", {{ id: "frm_1", sessionID: "{NATIVE}" }});
"#
        ));

        assert_eq!(
            summary(&reports),
            pairs(&[
                ("waitingForInput", "permission"),
                ("sessionStart", "resume"),
                ("turnStarted", "user"),
                ("waitingForInput", "permission"),
                ("turnStarted", "permission"),
                ("waitingForInput", "form"),
                ("turnStarted", "form"),
            ])
        );
    }

    #[test]
    fn a_new_conversation_in_the_same_terminal_is_reported_as_a_start() {
        let reports = reports(&format!(
            r#"
await emit("session.created", {{ sessionID: "{NATIVE}" }});
await emit("session.created", {{ sessionID: "ses_new" }});
await emit("session.execution.started", {{ sessionID: "ses_new" }});
"#
        ));

        assert_eq!(
            summary(&reports),
            pairs(&[
                ("sessionStart", "startup"),
                ("sessionStart", "startup"),
                ("turnStarted", "user"),
            ])
        );
        assert_eq!(reports[1]["nativeSessionId"], "ses_new");
    }

    /// The transcript path the plugin reports with `environment`.
    fn transcript_path(environment: &[(&str, &str)]) -> Option<String> {
        let reports = run(
            &json!({}),
            &json!([]),
            &format!(r#"await emit("session.created", {{ sessionID: "{NATIVE}" }});"#),
            environment,
        )
        .reports;
        assert_eq!(reports.len(), 1, "{reports:?}");
        reports[0]["transcriptPath"].as_str().map(str::to_owned)
    }

    #[test]
    fn the_transcript_is_the_database_wherever_opencode_resolves_it() {
        assert_eq!(
            transcript_path(&[("HOME", "/home/agent")]).as_deref(),
            Some("/home/agent/.local/share/opencode/opencode.db")
        );
        assert_eq!(
            transcript_path(&[("HOME", "/home/agent"), ("XDG_DATA_HOME", "/data")]).as_deref(),
            Some("/data/opencode/opencode.db")
        );
        // A relative name is resolved against OpenCode's data directory.
        assert_eq!(
            transcript_path(&[("HOME", "/home/agent"), ("OPENCODE_DB", "sessions/own.db")]).as_deref(),
            Some("/home/agent/.local/share/opencode/sessions/own.db")
        );
        assert_eq!(
            transcript_path(&[("HOME", "/home/agent"), ("OPENCODE_DB", "/var/own.db")]).as_deref(),
            Some("/var/own.db")
        );
        // A database in memory has nothing to read.
        assert_eq!(transcript_path(&[("OPENCODE_DB", ":memory:")]), None);
    }

    fn guarded(options: &Value) -> Vec<String> {
        let models = json!([
            model("aivar", "sigma2:Qwen3.8-27B", &["low", "medium", "xhigh"]),
            model("aivar", "aivar:borealis", &[]),
        ]);
        let run = run(
            options,
            &models,
            r"print(await context()); print(await context()); print(listed);",
            &[],
        );
        assert!(run.reports.is_empty(), "{:?}", run.reports);
        run.stdout
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("JSON").to_string())
            .collect()
    }

    #[test]
    fn a_configured_model_and_variant_pass_and_are_checked_once() {
        for options in [
            json!({"model": "aivar/sigma2:Qwen3.8-27B", "effort": "xhigh"}),
            json!({"model": "aivar/sigma2:Qwen3.8-27B"}),
            json!({"model": "aivar/aivar:borealis", "effort": "default"}),
        ] {
            assert_eq!(guarded(&options), [r#""allowed""#, r#""allowed""#, "1"], "{options}");
        }
    }

    #[test]
    fn without_a_selection_the_plugin_never_reads_the_models() {
        assert_eq!(guarded(&json!({})), [r#""allowed""#, r#""allowed""#, "0"]);
    }

    #[test]
    fn a_model_the_configuration_lacks_fails_every_request() {
        let refused = format!(
            "{:?}",
            "agentctl selected model aivar/nonexistent, which this Agent's OpenCode configuration does not define"
        );
        assert_eq!(
            guarded(&json!({"model": "aivar/nonexistent"})),
            [refused.as_str(), refused.as_str(), "2"]
        );
        // The provider is part of the name.
        let refused = format!(
            "{:?}",
            "agentctl selected model opencode/aivar:borealis, which this Agent's OpenCode configuration does not define"
        );
        assert_eq!(guarded(&json!({"model": "opencode/aivar:borealis"}))[0], refused);
    }

    /// The agents' models after the plugin's transforms, for agents `build` and `plan` without a
    /// model, a subagent `explore`, a primary `review` the Agent gave its own and a hidden `title`.
    fn agent_models(options: &Value) -> Value {
        let driver = r#"print(agents({
  build: { id: "build", mode: "primary" },
  plan: { id: "plan", mode: "all" },
  explore: { id: "explore", mode: "subagent" },
  review: { id: "review", mode: "primary", model: { providerID: "own", id: "model" } },
  title: { id: "title", mode: "primary", hidden: true },
}));"#;
        let run = run(options, &json!([]), driver, &[]);
        serde_json::from_str(run.stdout.trim()).expect("JSON agents")
    }

    #[test]
    fn the_selection_becomes_the_model_of_the_agents_the_operator_picks_without_their_own() {
        let selected = json!({"providerID": "aivar", "id": "azure:DeepSeek-V4.1-Flash", "variant": "low"});
        let agents = agent_models(&json!({"model": "aivar/azure:DeepSeek-V4.1-Flash", "effort": "low"}));
        assert_eq!(agents["build"]["model"], selected);
        assert_eq!(agents["plan"]["model"], selected);
        assert!(agents["explore"].get("model").is_none(), "{agents}");
        assert_eq!(agents["review"]["model"], json!({"providerID": "own", "id": "model"}));
        assert!(agents["title"].get("model").is_none(), "{agents}");

        let model_only = agent_models(&json!({"model": "aivar/aivar:borealis"}));
        assert_eq!(
            model_only["build"]["model"],
            json!({"providerID": "aivar", "id": "aivar:borealis"})
        );
        // Without a selection the agents keep OpenCode's choice.
        let unselected = agent_models(&json!({}));
        assert!(unselected["build"].get("model").is_none(), "{unselected}");
    }

    #[test]
    fn a_variant_the_model_lacks_fails_naming_the_ones_it_has() {
        assert_eq!(
            guarded(&json!({"model": "aivar/sigma2:Qwen3.8-27B", "effort": "high"}))[0],
            format!(
                "{:?}",
                "agentctl selected effort high for model aivar/sigma2:Qwen3.8-27B, which defines only the variants low, medium, xhigh"
            )
        );
        assert_eq!(
            guarded(&json!({"model": "aivar/aivar:borealis", "effort": "low"}))[0],
            format!(
                "{:?}",
                "agentctl selected effort low for model aivar/aivar:borealis, which defines no variants"
            )
        );
    }
}
