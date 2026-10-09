#![allow(clippy::expect_used)]

mod support;

use std::path::PathBuf;

use agent::{API_VERSION, EnvironmentSpec, Harness, KIND, SecretSpec, manifest};
use sandbox::RootFilesystemMode;

#[test]
fn decodes_the_minimal_manifest() {
    let bytes = include_bytes!("../examples/minimal/agent.yaml");
    let agent = manifest::decode(bytes).expect("minimal manifest should decode");

    assert_eq!(agent.api_version, API_VERSION);
    assert_eq!(agent.kind, KIND);
    assert_eq!(agent.metadata.name, "minimal");
    assert_eq!(agent.spec.sandbox.platform.os, "linux");
    assert_eq!(agent.spec.sandbox.platform.architecture, None);
    assert_eq!(agent.spec.sandbox.retention_policy, None);
    assert_eq!(agent.spec.harnesses.len(), 1);
    assert!(!agent.spec.harnesses[0].default);
    assert_eq!(
        agent.spec.default_harness().map(|harness| harness.kind),
        Some(Harness::ClaudeCode)
    );
    assert_eq!(
        agent.spec.sandbox.resources.root_filesystem().mode(),
        RootFilesystemMode::Layered
    );
}

#[test]
fn a_manifest_may_set_the_run_state_and_omits_it_by_default() {
    let minimal = include_str!("../examples/minimal/agent.yaml");
    let agent = manifest::decode(minimal.as_bytes()).expect("minimal manifest should decode");
    assert_eq!(agent.spec.run_state, None);
    assert!(!agent.spec.is_stopped());

    let stopped = minimal.replacen("spec:\n", "spec:\n  runState: Stopped\n", 1);
    let agent = manifest::decode(stopped.as_bytes()).expect("a Stopped run state should decode");
    assert_eq!(agent.spec.run_state, Some(agent::RunState::Stopped));
    let encoded = serde_yaml_ng::to_string(&agent).expect("encode");
    assert!(encoded.contains("runState: Stopped"), "{encoded}");

    let paused = minimal.replacen("spec:\n", "spec:\n  runState: Paused\n", 1);
    manifest::decode(paused.as_bytes()).expect_err("only Running and Stopped exist");
}

#[test]
fn decodes_explicit_non_secret_environment_with_an_optional_source() {
    let mut agent = support::agent("worker");
    agent.spec.environment = vec![
        EnvironmentSpec {
            name: "GIT_USER_NAME".into(),
            source: Some("HOST_GIT_NAME".into()),
        },
        EnvironmentSpec {
            name: "GIT_USER_EMAIL".into(),
            source: None,
        },
    ];

    let encoded = serde_yaml_ng::to_string(&agent).expect("encoded manifest");
    let decoded = manifest::decode(encoded.as_bytes()).expect("manifest environment");

    assert_eq!(decoded.spec.environment[0].name, "GIT_USER_NAME");
    assert_eq!(decoded.spec.environment[0].source(), "HOST_GIT_NAME");
    assert_eq!(decoded.spec.environment[1].source(), "GIT_USER_EMAIL");
}

#[test]
fn optional_secret_round_trips_without_changing_the_required_default() {
    let mut agent = support::agent("worker");
    agent.spec.secrets.push(SecretSpec {
        environment: "OPTIONAL_TOKEN".into(),
        optional: true,
        placeholder: None,
        allowed_hosts: vec!["example.com".into()],
        source: None,
    });

    let encoded = serde_yaml_ng::to_string(&agent).expect("encoded manifest");
    let decoded = manifest::decode(encoded.as_bytes()).expect("manifest with optional secret");
    assert!(decoded.spec.secrets[0].optional);
    assert!(encoded.contains("optional: true"));

    agent.spec.secrets[0].optional = false;
    let required = serde_yaml_ng::to_string(&agent).expect("encoded required secret");
    assert!(!required.contains("optional:"));
}

#[test]
fn rejects_invalid_duplicate_and_unpaired_environment_names() {
    let mut invalid = support::agent("worker");
    invalid.spec.environment.push(EnvironmentSpec {
        name: "NOT-PORTABLE".into(),
        source: None,
    });
    assert!(matches!(
        invalid.validate(),
        Err(agent::Error::Invalid(message)) if message.contains("spec.environment[0]")
    ));

    let mut duplicate = support::agent("worker");
    duplicate.spec.environment = vec![
        EnvironmentSpec {
            name: "EDITOR".into(),
            source: None,
        },
        EnvironmentSpec {
            name: "EDITOR".into(),
            source: Some("HOST_EDITOR".into()),
        },
    ];
    assert!(matches!(
        duplicate.validate(),
        Err(agent::Error::Invalid(message)) if message.contains("spec.environment[1]")
    ));

    let mut unpaired = support::agent("worker");
    unpaired.spec.environment.push(EnvironmentSpec {
        name: "GIT_USER_NAME".into(),
        source: None,
    });
    assert!(matches!(
        unpaired.validate(),
        Err(agent::Error::Invalid(message)) if message.contains("GIT_USER_NAME and GIT_USER_EMAIL")
    ));
}

#[test]
fn rejects_environment_collisions_with_secrets_and_harness_owned_values() {
    let mut secret_collision = support::agent("worker");
    secret_collision.spec.environment.push(EnvironmentSpec {
        name: "PLAIN_VALUE".into(),
        source: Some("SHARED_VALUE".into()),
    });
    secret_collision.spec.secrets.push(SecretSpec {
        environment: "API_TOKEN".into(),
        optional: false,
        placeholder: None,
        allowed_hosts: vec!["example.com".into()],
        source: Some("SHARED_VALUE".into()),
    });
    assert!(matches!(
        secret_collision.validate(),
        Err(agent::Error::Invalid(message)) if message.contains("spec.secrets[0]")
    ));

    let mut harness_collision = support::agent("worker");
    harness_collision.spec.environment.push(EnvironmentSpec {
        name: "CLAUDE_CONFIG_DIR".into(),
        source: None,
    });
    assert!(matches!(
        harness_collision.validate(),
        Err(agent::Error::Invalid(message)) if message.contains("spec.environment[0]")
    ));
}

#[test]
fn rejects_removed_repository_bootstrap_configuration() {
    let bytes = br"
apiVersion: agents.platform/v1alpha1
kind: Agent
metadata:
  name: worker
spec:
  repositories: []
";
    let error = manifest::decode(bytes).expect_err("repository bootstrap should not be part of the manifest");

    assert!(error.to_string().contains("repositories"));
}

#[test]
fn rejects_an_agent_name_that_cannot_identify_its_sandbox() {
    let agent = support::agent("Worker_Name");
    let error = agent.validate().expect_err("non-portable name should be rejected");

    assert!(matches!(error, agent::Error::Invalid(message) if message.starts_with("metadata.name:")));
}

#[test]
fn rejects_a_custom_placeholder_that_collides_with_a_generated_one() {
    let mut agent = support::agent("worker");
    agent.spec.secrets = vec![
        SecretSpec {
            environment: "FIRST_TOKEN".into(),
            optional: false,
            placeholder: None,
            allowed_hosts: vec!["example.com".into()],
            source: None,
        },
        SecretSpec {
            environment: "SECOND_TOKEN".into(),
            optional: false,
            placeholder: Some("$AGENT_SECRET_FIRST_TOKEN".into()),
            allowed_hosts: vec!["example.com".into()],
            source: None,
        },
    ];

    let error = agent
        .validate()
        .expect_err("effective placeholders must remain unambiguous");

    assert!(matches!(error, agent::Error::Invalid(message) if message.contains("spec.secrets[1]")));
}

#[test]
fn validates_harness_installation_cardinality_and_defaults() {
    let mut empty = support::agent("worker");
    empty.spec.harnesses.clear();
    assert!(matches!(
        empty.validate(),
        Err(agent::Error::Invalid(message)) if message.contains("spec.harnesses must not be empty")
    ));

    let installation = support::agent("worker").spec.harnesses.remove(0);
    let mut duplicate = support::agent("worker");
    let mut explicit_default = installation.clone();
    explicit_default.default = true;
    duplicate.spec.harnesses = vec![explicit_default, installation.clone()];
    assert!(matches!(
        duplicate.validate(),
        Err(agent::Error::Invalid(message)) if message.contains("duplicate harness kind")
    ));

    let mut codex = installation.clone();
    codex.kind = Harness::Codex;
    codex.version = Some("0.149.1".into());

    let mut no_default = support::agent("worker");
    no_default.spec.harnesses = vec![installation.clone(), codex.clone()];
    assert!(matches!(
        no_default.validate(),
        Err(agent::Error::Invalid(message)) if message.contains("exactly one default")
    ));

    let mut multiple_defaults = support::agent("worker");
    let mut first = installation;
    first.default = true;
    let mut second = codex;
    second.default = true;
    multiple_defaults.spec.harnesses = vec![first, second];
    assert!(matches!(
        multiple_defaults.validate(),
        Err(agent::Error::Invalid(message)) if message.contains("exactly one default")
    ));
}

#[test]
fn rejects_manifest_secrets_owned_by_a_declared_harness() {
    let mut agent = support::agent("worker");
    let mut codex = agent.spec.harnesses[0].clone();
    codex.kind = Harness::Codex;
    codex.version = Some("0.149.1".into());
    codex.default = false;
    agent.spec.harnesses[0].default = true;
    agent.spec.harnesses.push(codex);
    agent.spec.secrets.push(SecretSpec {
        environment: "AGENT_CODEX_ACCESS_TOKEN".into(),
        optional: false,
        placeholder: None,
        allowed_hosts: vec!["chatgpt.com".into()],
        source: None,
    });

    assert!(matches!(
        agent.validate(),
        Err(agent::Error::Invalid(message)) if message.contains("spec.secrets[0]")
    ));
}

#[test]
fn status_tolerates_unknown_fields_inside_provenance_and_conditions() {
    let status: agent::Status = serde_json::from_value(serde_json::json!({
        "observedGeneration": 1,
        "futureField": true,
        "provenance": {
            "sourceDirectory": "/source",
            "manifestPath": "/source/worker.yml",
            "futureField": "ignored"
        },
        "conditions": [{ "type": "Ready", "status": "True", "futureField": "ignored" }]
    }))
    .expect("newer status should decode");
    assert!(status.is_ready());
    let provenance = status.provenance.expect("provenance");
    assert_eq!(provenance.source_directory, std::path::Path::new("/source"));
    assert_eq!(
        provenance.manifest_path.as_deref(),
        Some(std::path::Path::new("/source/worker.yml"))
    );
}

#[test]
fn rejects_skills_without_a_directory_name_or_with_duplicate_names() {
    let mut agent = support::agent("worker");
    agent.spec.skills = vec![agent::SkillSpec {
        source: PathBuf::from("skills/.."),
        name: None,
    }];
    let error = agent.validate().expect_err("a source ending in .. has no skill name");
    assert!(matches!(error, agent::Error::Invalid(message) if message.starts_with("spec.skills[0]")));

    agent.spec.skills = vec![
        agent::SkillSpec {
            source: PathBuf::from("skills/evidence"),
            name: None,
        },
        agent::SkillSpec {
            source: PathBuf::from("../shared/evidence/"),
            name: None,
        },
    ];
    let error = agent
        .validate()
        .expect_err("two skills with the same directory name collide");
    assert!(
        matches!(error, agent::Error::Invalid(message) if message == "spec.skills[1] duplicates skill \"evidence\"")
    );

    agent.spec.skills.pop();
    agent.validate().expect("one named skill is valid");
    assert_eq!(agent.spec.skills[0].name(), Some("evidence"));

    agent.spec.skills[0].name = Some("installed-evidence".into());
    agent.validate().expect("an explicit skill name is valid");
    assert_eq!(agent.spec.skills[0].name(), Some("installed-evidence"));
}

#[test]
fn rejects_invalid_harness_default_selections() {
    let declared = manifest_with(&[(
        CLAUDE_CODE,
        "    - type: claudeCode\n      auth: mediated\n      defaults:\n        model: fable\n        effort: xhigh\n",
    )]);
    manifest::decode(&declared).expect("valid defaults decode");
    for (field, valid, invalid) in [
        ("model", "fable", "\"\""),
        ("effort", "xhigh", "\"\""),
        ("model", "fable", "\"gpt 5\""),
        ("effort", "xhigh", "\"hi'gh\""),
    ] {
        let yaml = String::from_utf8_lossy(&declared).replace(
            &format!("        {field}: {valid}\n"),
            &format!("        {field}: {invalid}\n"),
        );
        let error = manifest::decode(yaml.as_bytes()).expect_err("invalid selections are rejected");
        assert!(
            error
                .to_string()
                .contains(&format!("{field} must be 1-128 ASCII letters")),
            "{field} = {invalid}: {error}"
        );
    }
}

/// A manifest with one Claude Code installation and nothing optional.
const MANIFEST: &str = r#"
apiVersion: agents.platform/v1alpha1
kind: Agent
metadata:
  name: worker
spec:
  sandbox:
    image:
      type: reference
      reference: example.invalid/agent:latest
    platform:
      os: linux
    resources:
      cpu: "2"
      memory: "1Gi"
      rootFilesystem:
        capacity: "4Gi"
        mode: layered
  home:
    source: home
  harnesses:
    - type: claudeCode
      auth: mediated
  network:
    mode: mediated
    allow: all
"#;

/// The lines of `MANIFEST` the cases below replace or extend.
const CLAUDE_CODE: &str = "    - type: claudeCode\n      auth: mediated\n";
const ROOT_FILESYSTEM_MODE: &str = "        mode: layered\n";
const NETWORK: &str = "  network:\n";

/// `MANIFEST` with each `(line, replacement)` applied once.
fn manifest_with(replacements: &[(&str, &str)]) -> Vec<u8> {
    replacements
        .iter()
        .fold(MANIFEST.to_owned(), |manifest, (line, replacement)| {
            assert!(manifest.contains(line), "{line:?}");
            manifest.replacen(line, replacement, 1)
        })
        .into_bytes()
}

fn manifest_with_access(access: &str) -> Vec<u8> {
    manifest_with(&[(NETWORK, &format!("{access}{NETWORK}"))])
}

/// The Agent resource is stored and sent over the Control API in its serialized form, so a field
/// left at its default stays out of it: a manifest that never sets the field is unchanged by it.
#[test]
fn decodes_each_part_of_a_manifest_and_serializes_only_what_it_sets() {
    type Check = fn(&agent::Agent, &serde_json::Value);
    let cases: [(&str, Vec<u8>, Check); 6] = [
        (
            "Sandbox Mounts",
            manifest_with(&[(
                ROOT_FILESYSTEM_MODE,
                "        mode: layered\n    mounts:\n      - type: bind\n        source: ../..\n        \
                 target: /home/agent/code/altinn-studio\n        readOnly: false\n      - type: tmpfs\n        \
                 target: /tmp\n        capacity: \"1Gi\"\n",
            )]),
            |_, value| {
                assert_eq!(value["spec"]["sandbox"]["mounts"][0]["source"], "../..");
                assert_eq!(value["spec"]["sandbox"]["mounts"][1]["capacity"], "1Gi");
            },
        ),
        (
            "a harness without a declared version, and no access",
            manifest_with(&[]),
            |agent, value| {
                assert_eq!(agent.spec.harnesses[0].version, None);
                assert!(value["spec"]["harnesses"][0].get("version").is_none());
                assert!(value["spec"]["harnesses"][0].get("optional").is_none());
                assert!(agent.spec.access.is_empty() && !agent.spec.ssh_access());
                assert!(value["spec"].get("access").is_none(), "an empty list is not serialized");
            },
        ),
        (
            "an optional harness installation",
            manifest_with(&[(
                CLAUDE_CODE,
                "    - type: claudeCode\n      auth: mediated\n      default: true\n    \
                 - type: codex\n      auth: mediated\n      optional: true\n",
            )]),
            |agent, value| {
                assert!(!agent.spec.harness(Harness::ClaudeCode).expect("Claude Code").optional);
                assert!(agent.spec.harness(Harness::Codex).expect("Codex").optional);
                assert!(value["spec"]["harnesses"][0].get("optional").is_none());
                assert_eq!(value["spec"]["harnesses"][1]["optional"], true);
            },
        ),
        (
            "model and effort defaults per installation",
            manifest_with(&[(
                CLAUDE_CODE,
                "    - type: claudeCode\n      auth: mediated\n      default: true\n      defaults:\n        \
                 model: fable\n        effort: xhigh\n    - type: codex\n      auth: mediated\n      \
                 defaults:\n        model: gpt-5.4-codex\n",
            )]),
            |agent, value| {
                let (claude, codex) = (&agent.spec.harnesses[0].defaults, &agent.spec.harnesses[1].defaults);
                assert_eq!(
                    (claude.model_str(), claude.effort_str()),
                    (Some("fable"), Some("xhigh"))
                );
                assert_eq!((codex.model_str(), codex.effort_str()), (Some("gpt-5.4-codex"), None));
                assert_eq!(value["spec"]["harnesses"][0]["defaults"]["effort"], "xhigh");
                assert!(value["spec"]["harnesses"][1]["defaults"].get("effort").is_none());
            },
        ),
        (
            "SSH access",
            manifest_with_access("  access:\n    - type: ssh\n"),
            |agent, value| {
                assert_eq!(agent.spec.access, vec![agent::AccessSpec::Ssh {}]);
                assert!(agent.spec.ssh_access());
                assert!(!agent.spec.vnc_access(), "one capability does not imply the other");
                assert_eq!(value["spec"]["access"], serde_json::json!([{"type": "ssh"}]));
            },
        ),
        (
            "VNC access beside SSH",
            manifest_with_access("  access:\n    - type: ssh\n    - type: vnc\n"),
            |agent, value| {
                assert!(agent.spec.ssh_access() && agent.spec.vnc_access());
                assert_eq!(
                    value["spec"]["access"],
                    serde_json::json!([{"type": "ssh"}, {"type": "vnc"}])
                );
            },
        ),
    ];

    for (what, bytes, check) in cases {
        let agent = manifest::decode(&bytes).unwrap_or_else(|error| panic!("{what}: {error}"));
        let value = serde_json::to_value(&agent).expect("Agent JSON");
        check(&agent, &value);
    }
}

#[test]
fn rejects_unknown_duplicate_and_configured_access_capabilities() {
    assert!(matches!(
        manifest::decode(&manifest_with_access("  access:\n    - type: ssh\n    - type: ssh\n")),
        Err(agent::Error::Invalid(message)) if message.contains("spec.access[1]")
    ));
    assert!(matches!(
        manifest::decode(&manifest_with_access("  access:\n    - type: vnc\n    - type: vnc\n")),
        Err(agent::Error::Invalid(message)) if message.contains("spec.access[1]")
    ));
    assert!(matches!(
        manifest::decode(&manifest_with_access("  access:\n    - type: rdp\n")),
        Err(agent::Error::Yaml(_))
    ));
    assert!(
        matches!(
            manifest::decode(&manifest_with_access("  access:\n    - type: vnc\n      port: 5901\n")),
            Err(agent::Error::Yaml(_))
        ),
        "VNC access exposes no tunables"
    );
    assert!(
        matches!(
            manifest::decode(&manifest_with_access("  access:\n    - type: ssh\n      port: 22\n")),
            Err(agent::Error::Yaml(_))
        ),
        "SSH access exposes no tunables"
    );
    // `access` belongs to the Agent, not the Sandbox.
    let nested = manifest_with(&[(
        ROOT_FILESYSTEM_MODE,
        "        mode: layered\n    access:\n      - type: ssh\n",
    )]);
    assert!(matches!(manifest::decode(&nested), Err(agent::Error::Yaml(_))));
}
