# Starter pipelines

Working pipelines to install, run, and edit. Copy one, change it, install it
under a new `name:` — or ignore them and write YAML from scratch.

These files are seeded on first run into the **parent** of the `tools.data_dir`
in your config — `~/.agentos/pipelines/core/` under the standard installer,
`/tmp/agentos/pipelines/core/` under the shipped `config/default.toml`, or
`$AGENTOS_DATA_DIR/pipelines/core/` when that variable is set.
`agentos config get tools.data_dir` prints the one in force — the templates sit
beside it, one level up.

The kernel installs them into the pipeline store on boot, so they appear in
`agentos pipeline list` and in the panel's Pipelines section without any setup.
A name already in the store is left alone — your own edit under the same name is
never overwritten. One you `pipeline remove` comes back on the next restart;
rename your copy if you want it gone for good.

## Run one

```bash
agentos agent list                        # pick the agent that will run the steps
agentos pipeline run hello-pipeline --agent <your-agent> --input "quantum computing"
```

Edited a file in `pipelines/core/` by hand? Re-install it to push the change into
the store: `agentos pipeline install ~/.agentos/pipelines/core/01-hello-pipeline.yaml`.

`--agent` is required: it is the agent whose permissions govern every step,
including tool steps. A step is denied when that agent lacks the permission the
tool declares (`agentos perm grant <agent> network.outbound:x`).

## The five

| File | Teaches |
|------|---------|
| `01-hello-pipeline.yaml` | One agent step, `{{input}}`, `output` |
| `02-research-report.yaml` | Tool steps vs agent steps, `depends_on`, `output_var` |
| `03-parallel-review.yaml` | Wave execution — independent steps run concurrently |
| `04-resilient-fetch.yaml` | `retry_on_failure`, backoff, `on_failure: fail / skip / use_default` |
| `05-daily-digest.yaml` | Tool → agent → tool, memory search, `--detach` and what it cannot do |

Read them in order; each one adds one idea to the one before it.

## Three things that surprise everyone

- **A tool step's output is the tool's whole result JSON, as a string** — not
  prose. Agent steps cope with that; another tool step's payload may not.
- **Interpolated values are wrapped.** In an agent prompt, `{{input}}` and every
  step output arrive as `<user_data>…</user_data>`; only `{{run_id}}`, `{{date}}`
  and `{{timestamp}}` go in verbatim. That is injection defence, not a bug.
- **`max_cost_usd` and `max_wall_time_minutes` parse but are not enforced.** The
  only real limits are per-step `timeout_minutes` and the agent's daily budget.
  Kernel-action tools (`notify-user`, `ask-user`, `spawn-agent`) cannot be a
  `tool:` step at all, and under `--detach` an agent step is a single inference
  with no tool loop — see the header of `05-daily-digest.yaml`.

## Built-in variables

`{{name}}` resolves against these, plus every `output_var` a completed step has
set. A reference that resolves to nothing is left in place as
`{{UNRESOLVED:name}}` rather than blanked, so a mistake is visible in the step
logs instead of silently producing an empty prompt.

| Variable | Value |
|----------|-------|
| `{{input}}` | The `--input` string |
| `{{agent}}` | The `--agent` name — what makes these templates run unedited |
| `{{run_id}}` | This run's UUID — useful for unique output paths |
| `{{date}}` | `YYYY-MM-DD`, UTC |
| `{{timestamp}}` | Unix seconds |

`{{agent}}` is substituted in a step's `agent:` field only when it is the whole
value. `agent: "review-{{agent}}"` is passed through untouched and looked up
literally.

## Editing one

1. Copy it: `cp ~/.agentos/pipelines/core/02-research-report.yaml my-report.yaml`
2. Change `name:` — install overwrites by name, and `research-report` is taken.
3. `agentos pipeline install my-report.yaml`, then run it.

A tool step's `input` must match that tool's payload schema — read it in
`~/.agentos/tools/core/<tool>.toml` under `[payload_schema]`.
`agentos pipeline list` shows what is installed; `agentos pipeline remove <name>`
takes one out.

Full reference: `docs/handbook/11-Pipeline and Workflows.md`.
