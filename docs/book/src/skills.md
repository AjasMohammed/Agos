# Skills

A **skill** packages a system prompt ("recipe") plus metadata — trust tier, triggers,
expected tools, and budget caps — into a directory containing a `SKILL.toml` manifest and a
prompt file. Skills let users and agents author, install, run, and fetch reusable recipes at
runtime.

## Where skills live

Two roots, configured under `[skills]`:

```toml
[skills]
core_skills_dir = "skills/core"   # distribution-shipped skills
user_skills_dir = "skills/user"   # user- and agent-authored skills
```

Both are scanned at kernel boot into the `SkillRegistry`. Core ships eight skills:
`alert-builder`, `backup-guardian`, `browser-automator`, `compliance-auditor`,
`cost-optimizer`, `infra-watcher`, `researcher`, and `secops-monitor`.

A live snapshot mirrors the registry so the `agent-manual` and `skill-prompt` tools can read
the installed skills without touching the registry directly. The snapshot is refreshed on
every install/remove.

## Manifest

`SKILL.toml` (type `SkillManifest` in `agentos-types/src/skill.rs`) declares the skill name,
trust tier, triggers (`SkillTriggers`), the agent binding (`SkillAgent`), expected tools
(`SkillTools`), required permissions (`SkillPermissions`), and budget caps (`SkillBudget`).
Community/Verified skills are signed like tool manifests.

## CLI

| Command | Effect |
|---------|--------|
| `agentos skill install <path>` | Load and register a skill from a directory. |
| `agentos skill remove <name>` | Uninstall a skill. |
| `agentos skill list` | List installed skills. |

At runtime, an agent fetches a skill's prompt via the `skill-prompt` tool and discovers
available skills through `agent-manual`.

## Implementation

- Types: `agentos-types/src/skill.rs`
- Registry/loader: `agentos-skills/src/{registry.rs,manifest.rs}`
- Kernel handlers: `agentos-kernel/src/commands/skill.rs`
- Installer adapter: `agentos-kernel/src/skill_installer.rs`
