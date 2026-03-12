# Obsidian Planning Rule

> **Mandatory for all non-trivial planning, design, and result documentation.**

---

## 1. Core Principle

All implementation plans, design documents, and execution results MUST be stored within the project's Obsidian vault. This ensures a persistent, human-readable, and searchable record of all major agent activities.

## 2. Vault Configuration

- **Vault Path**: `/home/ajas/Desktop/agos/obsidian-vault`
- **Mandatory Skill**: Must use the `obsidian-markdown` skill for all vault interactions.

## 3. Organization and Structure

Each task or significant feature development must have its own dedicated folder within the `tasks/` directory (or a relevant feature-specific root).

### Folder Hierarchy

```
obsidian-vault/
└── tasks/
    └── <yyyy-mm-dd>-<task-slug>/
        ├── index.md            # Detailed task description and purpose
        ├── task-plan.md        # The approved implementation plan
        ├── task-result.md      # Execution summary and verification results
        └── task-<extra>.md     # Supporting notes, diagrams, or findings
```

### File Naming Conventions

- **Distinguishability**: All files must be identifiable in a global search. Avoid generic names like `plan.md`. Use the `task-` prefix or include the task name in the filename.
- **Format**: Use `kebab-case` for folder and filenames.

## 4. Content Requirements

- **Frontmatter**: Every file must include appropriate YAML frontmatter (title, date, tags, status).
- **Interlinking**: Use Obsidian wikilinks (`[[Note]]`) to connect related tasks, results, and architecture notes.
- **Index File**: The `index.md` must provide context: what is being done, why, and how it relates to the overall project.

## 5. Workflow Integration

1.  **Planning**: Create the task folder, `index.md`, and `task-plan.md` in the vault.
2.  **Execution**: Update the vault as work progresses if new insights are found.
3.  **Completion**: Create `task-result.md` with proof of work, test logs, and any final notes.
4.  **Verification**: Ensure all links are valid and the folder structure is clean.

---

> [!IMPORTANT]
> This rule overrides default "brain" artifact usage for persistent project documentation. Internal artifacts (`task.md`, `implementation_plan.md`, `walkthrough.md`) should still be used for the agent's internal state tracking but MUST be mirrored or summarized in the vault for the user.
