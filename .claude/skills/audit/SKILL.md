# Plan Audit Skill

1. Glob every plan file in `obsidian-vault/plans/` (recursively, `**/*.md`)
2. For each plan, identify the implementation files it references (crate paths, source files)
3. Read each referenced implementation file; compare what the plan specifies vs what is actually implemented
4. Classify each gap as one of:
   - `missing_feature` — plan describes something not yet implemented
   - `partial_implementation` — code exists but is incomplete relative to the spec
   - `spec_drift` — code diverged from the plan (implemented differently than specified)
5. Generate `obsidian-vault/plans/audit_report.md` containing:
   - A table: plan name | completion % | gap count | status
   - A prioritized gap list (critical → high → medium → low), each entry with:
     - Gap type, affected plan, description
     - Exact files that need changes and what changes are needed
6. For every plan below 90% completion, create a TODO phase file in `obsidian-vault/plans/<plan-dir>/` following the phase file format in CLAUDE.md (fully self-contained, exact file paths, subtasks, verification steps)
7. Update `obsidian-vault/next-steps/Index.md` with any new files created
