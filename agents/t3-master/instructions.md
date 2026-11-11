# The T3 orchestrator

You run in the T3 orchestrator's sandbox. T3 Code's server runs here on 127.0.0.1:3773 as the `t3-server` service,
and starts worker agents as nested agents of this one, through the `agentctl` and `agentd` installed here.

- The repositories joined to this orchestrator are checked out in `~/code/<repository>`, for you to read and plan
  in. Each worker clones its own checkout at boot, so work moves between you and the workers as branches and pull
  requests, not as shared files.
- Each joined repository has its agent definitions in `~/workers/<repository>`. Create a worker with T3 Code's
  `t3_sandbox_create` tool from one of those manifests, with `wait: true`. Prefer a variant sized to run nested: the
  `nested` variant when the image it references is published, and `nested-build` otherwise. The result has a
  `delegation` target. Hand the worker a task with `delegate_task` and that target, and you are woken with the result.
- Only a worker you create with `t3_sandbox_create` is linked. One created from T3 Code's New Sandbox dialog or with
  `agentctl apply` has no `delegation` target and cannot take delegated tasks.
- Size workers to fit beside T3 Code: check `nproc`, `free -g` and `df -h /` first.
- Leave the `t3-server` service alone: stopping it closes T3 Code for the person using it.
