#![allow(clippy::expect_used)]

use crate::support;

use std::path::Path;

use agent::{AgentVariantName, manifest};

fn agent_directory() -> support::TempDirectory {
    let directory = support::TempDirectory::new("variants");
    let yaml = serde_yaml_ng::to_string(&support::agent("base")).expect("base YAML");
    std::fs::write(directory.path().join("agent.yaml"), yaml).expect("base manifest");
    directory
}

fn write(directory: &Path, name: &str, body: &str) {
    std::fs::write(directory.join(name), body).expect("variant manifest");
}

fn variant(extends: &str, name: &str, spec: &str) -> String {
    format!(
        "apiVersion: agents.platform/v1alpha1\nkind: AgentVariant\nextends: {extends}\nmetadata:\n  name: {name}\n{spec}"
    )
}

#[test]
fn resolves_one_level_and_multilevel_variants_from_the_base_outward() {
    let directory = agent_directory();
    write(
        directory.path(),
        "agent.nested.yaml",
        &variant(
            "agent.yaml",
            "nested",
            "spec:\n  sandbox:\n    resources:\n      cpu: '1'\n      memory: 2Gi\n",
        ),
    );
    write(
        directory.path(),
        "agent.mine.yaml",
        &variant(
            "agent.nested.yaml",
            "mine",
            "spec:\n  sandbox:\n    resources:\n      cpu: '3'\n",
        ),
    );

    let resolved = manifest::resolve(&directory.path().join("agent.mine.yaml")).expect("resolved variant");
    let value = serde_json::to_value(&resolved.agent).expect("Agent JSON");
    assert_eq!(resolved.agent.metadata.name, "mine");
    assert_eq!(value["spec"]["sandbox"]["resources"]["cpu"], "3");
    assert_eq!(value["spec"]["sandbox"]["resources"]["memory"], "2Gi");
    assert_eq!(
        resolved
            .chain
            .iter()
            .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
            .collect::<Vec<_>>(),
        ["agent.mine.yaml", "agent.nested.yaml", "agent.yaml"]
    );
}

/// A team variant that fills every keyed list, for a personal variant to change one entry of each.
const TEAM_SPEC: &str = "spec:
  access:
    - type: ssh
    - type: vnc
  harnesses:
    - type: codex
      auth: mediated
      default: true
  environment:
    - name: A
    - name: B
      source: B_FILE
  secrets:
    - environment: TOKEN
      allowedHosts: [api.github.com]
  skills:
    - source: skills/review
  sandbox:
    mounts:
      - type: tmpfs
        target: /tmp
        capacity: 1Gi
";

#[test]
fn keyed_lists_merge_by_key_while_other_lists_replace() {
    let directory = agent_directory();
    write(
        directory.path(),
        "agent.team.yaml",
        &variant("agent.yaml", "team", TEAM_SPEC),
    );
    write(
        directory.path(),
        "agent.mine.yaml",
        &variant(
            "agent.team.yaml",
            "mine",
            "spec:
  instructions:
    - source: mine.md
  harnesses:
    - type: claudeCode
      optional: true
  environment:
    - name: B
      source: null
    - name: C
  secrets:
    - environment: TOKEN
      optional: true
  skills:
    - name: review
      optional: true
  sandbox:
    mounts:
      - type: bind
        target: /work
        source: work
        readOnly: true
      - type: tmpfs
        target: /tmp
        capacity: 2Gi
",
        ),
    );

    let agent = manifest::resolve(&directory.path().join("agent.mine.yaml"))
        .expect("keyed variant")
        .agent;
    let value = serde_json::to_value(&agent).expect("Agent JSON");
    let spec = &value["spec"];
    // The base's Claude Code keeps its place and its version, and the team's Codex follows it.
    assert_eq!(spec["harnesses"][0]["type"], "claudeCode");
    assert_eq!(spec["harnesses"][0]["version"], "2.1.266");
    assert_eq!(spec["harnesses"][0]["optional"], true);
    assert_eq!(spec["harnesses"][1]["type"], "codex");
    assert_eq!(
        spec["environment"],
        serde_json::json!([{"name": "A"}, {"name": "B"}, {"name": "C"}])
    );
    assert_eq!(
        spec["secrets"][0]["allowedHosts"],
        serde_json::json!(["api.github.com"])
    );
    assert_eq!(spec["secrets"][0]["optional"], true);
    assert_eq!(
        spec["skills"],
        serde_json::json!([{"source": "skills/review", "name": "review", "optional": true}])
    );
    assert_eq!(spec["sandbox"]["mounts"][0]["target"], "/tmp");
    assert_eq!(spec["sandbox"]["mounts"][0]["capacity"], "2Gi");
    assert_eq!(spec["sandbox"]["mounts"][1]["target"], "/work");
    assert_eq!(spec["access"], serde_json::json!([{"type": "ssh"}, {"type": "vnc"}]));
    assert_eq!(spec["instructions"], serde_json::json!([{"source": "mine.md"}]));
}

#[test]
fn a_patch_delete_entry_removes_the_inherited_one_and_an_empty_list_clears_them() {
    let directory = agent_directory();
    write(
        directory.path(),
        "agent.team.yaml",
        &variant("agent.yaml", "team", TEAM_SPEC),
    );
    write(
        directory.path(),
        "agent.mine.yaml",
        &variant(
            "agent.team.yaml",
            "mine",
            "spec:
  access:
    - type: vnc
      $patch: delete
  environment:
    - name: A
      $patch: delete
    - name: GONE_FROM_THE_BASE
      $patch: delete
  skills: []
  instructions: []
",
        ),
    );

    let agent = manifest::resolve(&directory.path().join("agent.mine.yaml"))
        .expect("removing variant")
        .agent;
    assert_eq!(agent.spec.access, [agent::manifest::AccessSpec::Ssh {}]);
    assert_eq!(
        agent
            .spec
            .environment
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["B"]
    );
    assert_eq!(agent.spec.skills, []);
    assert_eq!(agent.spec.instructions, []);
    assert_eq!(agent.spec.home.source, Path::new("home"));
}

/// An agent serves several web views, so a variant addresses one by its `type` and `name`.
#[test]
fn web_views_merge_by_type_and_name() {
    let directory = agent_directory();
    write(
        directory.path(),
        "agent.team.yaml",
        &variant(
            "agent.yaml",
            "team",
            "spec:
  access:
    - type: ssh
    - type: web
      name: t3code
      port: 3773
    - type: web
      name: docs
      port: 8080
",
        ),
    );
    write(
        directory.path(),
        "agent.mine.yaml",
        &variant(
            "agent.team.yaml",
            "mine",
            "spec:
  access:
    - type: web
      name: t3code
      port: 3774
    - type: web
      name: docs
      $patch: delete
    - type: web
      name: novnc
      port: 6080
      path: /vnc.html
",
        ),
    );

    let agent = manifest::resolve(&directory.path().join("agent.mine.yaml"))
        .expect("web view variant")
        .agent;
    let web = |name: &str, port: u16, path: &str| agent::manifest::AccessSpec::Web {
        name: name.into(),
        port,
        path: path.into(),
    };
    assert_eq!(
        agent.spec.access,
        [
            agent::manifest::AccessSpec::Ssh {},
            web("t3code", 3774, "/"),
            web("novnc", 6080, "/vnc.html"),
        ]
    );
}

#[test]
fn a_keyed_entry_of_another_type_replaces_the_inherited_one_whole() {
    let directory = agent_directory();
    write(
        directory.path(),
        "agent.team.yaml",
        &variant("agent.yaml", "team", TEAM_SPEC),
    );
    write(
        directory.path(),
        "agent.mine.yaml",
        &variant(
            "agent.team.yaml",
            "mine",
            "spec:\n  sandbox:\n    mounts:\n      - type: bind\n        target: /tmp\n        source: tmp\n        readOnly: false\n",
        ),
    );

    let agent = manifest::resolve(&directory.path().join("agent.mine.yaml"))
        .expect("retyped mount")
        .agent;
    assert!(matches!(
        agent.spec.sandbox.mounts.as_slice(),
        [agent::manifest::MountSpec::Bind { read_only: false, .. }]
    ));
}

#[test]
fn refuses_patch_markers_a_variant_cannot_apply() {
    let directory = agent_directory();
    for (name, spec, expected) in [
        (
            "replace",
            "spec:\n  access:\n    - type: ssh\n      $patch: replace\n",
            "must be \"delete\"",
        ),
        (
            "nameless",
            "spec:\n  environment:\n    - $patch: delete\n",
            "spec.environment: an entry with $patch must name the entry it removes by its name",
        ),
        (
            "unkeyed",
            "spec:\n  instructions:\n    - source: a.md\n      $patch: delete\n",
            "$patch",
        ),
    ] {
        let file = format!("agent.{name}.yaml");
        write(directory.path(), &file, &variant("agent.yaml", name, spec));
        let error = manifest::resolve(&directory.path().join(&file)).expect_err(name);
        assert!(error.to_string().contains(expected), "{name}: {error}");
        assert!(error.to_string().contains(&file), "{name}: {error}");
    }

    // A complete agent manifest has nothing to remove from, so a marker in it is an unknown field.
    let mut base = serde_yaml_ng::to_value(support::agent("complete")).expect("agent YAML");
    base["spec"]["access"] = serde_yaml_ng::from_str("[{type: ssh, $patch: delete}]").expect("access");
    std::fs::write(
        directory.path().join("agent.yaml"),
        serde_yaml_ng::to_string(&base).expect("agent YAML"),
    )
    .expect("complete manifest");
    let error = manifest::resolve(&directory.path().join("agent.yaml")).expect_err("marker in a complete agent");
    assert!(error.to_string().contains("$patch"), "{error}");
}

#[test]
fn changing_a_tagged_mapping_type_replaces_the_previous_variant() {
    let directory = agent_directory();
    write(
        directory.path(),
        "agent.reference.yaml",
        &variant(
            "agent.yaml",
            "reference",
            "spec:\n  sandbox:\n    image:\n      type: reference\n      reference: example.invalid/agent:latest\n",
        ),
    );

    let agent = manifest::resolve(&directory.path().join("agent.reference.yaml"))
        .expect("tagged mapping replacement")
        .agent;
    assert!(matches!(
        agent.spec.sandbox.image,
        sandbox::image::ImageSource::Reference { .. }
    ));
}

#[test]
fn null_removes_optional_fields_and_required_removal_fails_final_validation() {
    let directory = agent_directory();
    write(
        directory.path(),
        "agent.optional.yaml",
        &variant(
            "agent.yaml",
            "optional",
            "spec:\n  sandbox:\n    retentionPolicy: null\n",
        ),
    );
    let optional = manifest::resolve(&directory.path().join("agent.optional.yaml")).expect("optional removal");
    assert_eq!(optional.agent.spec.sandbox.retention_policy, None);

    write(
        directory.path(),
        "agent.required.yaml",
        &variant("agent.yaml", "required", "spec:\n  sandbox:\n    resources: null\n"),
    );
    let error = manifest::resolve(&directory.path().join("agent.required.yaml")).expect_err("required removal");
    assert!(error.to_string().contains("resources"));
    assert!(error.to_string().contains("agent.required.yaml"));
}

#[test]
fn rejects_unknown_variant_fields_even_when_null() {
    let directory = agent_directory();
    for (name, spec) in [
        ("unknown", "spec:\n  mystery: null\n"),
        (
            "nested-unknown",
            "spec:\n  sandbox:\n    resources:\n      mystery: null\n",
        ),
    ] {
        let filename = format!("agent.{name}.yaml");
        write(directory.path(), &filename, &variant("agent.yaml", name, spec));
        let error = manifest::resolve(&directory.path().join(filename)).expect_err("unknown field");
        assert!(error.to_string().contains("unknown field"), "{error}");
    }
}

#[test]
fn rejects_missing_bases_cross_directory_paths_and_absolute_paths() {
    let directory = agent_directory();
    for (name, extends, expected) in [
        ("missing", "agent.absent.yaml", "could not read"),
        ("parent", "../agent.yaml", "extends must name"),
        ("absolute", "/tmp/agent.yaml", "extends must name"),
    ] {
        let filename = format!("agent.{name}.yaml");
        write(directory.path(), &filename, &variant(extends, name, ""));
        let error = manifest::resolve(&directory.path().join(filename)).expect_err("invalid base");
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn reports_the_complete_cycle() {
    let directory = agent_directory();
    write(
        directory.path(),
        "agent.one.yaml",
        &variant("agent.two.yaml", "one", ""),
    );
    write(
        directory.path(),
        "agent.two.yaml",
        &variant("agent.one.yaml", "two", ""),
    );
    let error = manifest::resolve(&directory.path().join("agent.one.yaml")).expect_err("cycle");
    assert!(
        error
            .to_string()
            .contains("agent.one.yaml -> agent.two.yaml -> agent.one.yaml"),
        "{error}"
    );
}

#[test]
fn base_errors_include_the_complete_inheritance_chain() {
    let directory = agent_directory();
    write(
        directory.path(),
        "agent.parent.yaml",
        &variant("agent.missing.yaml", "parent", ""),
    );
    write(
        directory.path(),
        "agent.leaf.yaml",
        &variant("agent.parent.yaml", "leaf", ""),
    );

    let error = manifest::resolve(&directory.path().join("agent.leaf.yaml")).expect_err("missing base");
    let message = error.to_string();
    assert!(message.contains("inheritance chain:"), "{message}");
    assert!(message.contains("agent.leaf.yaml\n  extends agent.parent.yaml\n  extends agent.missing.yaml"));
}

#[test]
fn limits_inheritance_to_sixteen_manifests() {
    let directory = agent_directory();
    for index in 1..=16 {
        let extends = if index == 16 {
            "agent.yaml".to_owned()
        } else {
            format!("agent.v{}.yaml", index + 1)
        };
        write(
            directory.path(),
            &format!("agent.v{index}.yaml"),
            &variant(&extends, &format!("v{index}"), ""),
        );
    }
    let error = manifest::resolve(&directory.path().join("agent.v1.yaml")).expect_err("depth limit");
    assert!(error.to_string().contains("exceeds 16 manifests"), "{error}");
}

#[test]
fn requires_names_and_matching_api_versions_in_every_variant() {
    let directory = agent_directory();
    write(
        directory.path(),
        "agent.nameless.yaml",
        "apiVersion: agents.platform/v1alpha1\nkind: AgentVariant\nextends: agent.yaml\nmetadata: {}\n",
    );
    let error = manifest::resolve(&directory.path().join("agent.nameless.yaml")).expect_err("name required");
    assert!(error.to_string().contains("metadata.name"));

    write(
        directory.path(),
        "agent.version.yaml",
        "apiVersion: agents.platform/v2\nkind: AgentVariant\nextends: agent.yaml\nmetadata:\n  name: version\n",
    );
    let error = manifest::resolve(&directory.path().join("agent.version.yaml")).expect_err("version mismatch");
    assert!(error.to_string().contains("apiVersion"));
}

#[test]
fn validates_filename_grammar() {
    let nested_build: AgentVariantName = "nested-build".parse().expect("variant name");
    assert_eq!(nested_build.as_str(), "nested-build");
    assert_eq!(nested_build.filename(), "agent.nested-build.yaml");
    assert_eq!(
        serde_json::to_string(&nested_build).expect("serialized name"),
        r#""nested-build""#
    );
    assert_eq!(
        serde_json::from_str::<AgentVariantName>(r#""nested-build""#).expect("deserialized name"),
        nested_build
    );

    for accepted in [
        "agent.nested.yaml",
        "agent.nested-build.yaml",
        "agent.worktree.yaml",
        "agent.mine.yaml",
        "agent.large-local.yaml",
    ] {
        assert!(manifest::is_manifest_filename(Path::new(accepted)), "{accepted}");
    }
    assert!(manifest::is_manifest_filename(Path::new("agent.yaml")));
    for rejected in [
        "agent-copy.yaml",
        "agent..yaml",
        "agent.Nested.yaml",
        "my-agent.yaml",
        "agent.yaml.bak",
        "agent.trailing-.yaml",
    ] {
        assert!(!manifest::is_manifest_filename(Path::new(rejected)), "{rejected}");
    }
    for rejected in ["", "Nested", "nested_build", "nested-", "../nested"] {
        assert!(rejected.parse::<AgentVariantName>().is_err(), "{rejected}");
    }
}
