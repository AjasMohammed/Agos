---
name: obsidian-markdown
description: Create, edit, and refactor Obsidian-compatible Markdown notes while preserving vault conventions. Use when requests mention Obsidian, vault notes, wikilinks, backlinks, daily notes, YAML frontmatter, note templates, tags, callouts, tasks, or bulk Markdown updates that must stay compatible with Obsidian.
---

# Obsidian Markdown

## Overview

Use this skill to work on `.md` files in an Obsidian vault without breaking links or metadata.
Preserve existing note structure, link style, and frontmatter conventions unless the user asks to change them.

## Workflow

1. Inspect nearby notes before editing.
2. Match the vault's conventions for filenames, folders, frontmatter keys, tags, and link style.
3. Apply the smallest change that solves the request.
4. Validate links and note structure after edits.

## Core Rules

- Keep YAML frontmatter at the top and preserve unknown keys.
- Keep Obsidian link syntax as-is: `[[Note]]`, `[[Note#Heading]]`, `[[Note|Alias]]`, `![[Embed]]`.
- Keep block references such as `^block-id` when present.
- Keep callouts, tasks, and comments in valid Obsidian Markdown format.
- Avoid broad rewrites that may remove spacing, anchors, or list indentation.

## Task Patterns

### Create Notes

- Create the note in the requested folder.
- Follow the vault's naming style (date-based, title-based, or mixed).
- Include frontmatter only if the vault uses it.
- Add initial outbound links where context is known.

### Edit Existing Notes

- Modify only requested sections when possible.
- Keep heading hierarchy stable unless asked to reorganize.
- Preserve existing wikilinks, embeds, and tag conventions.
- Keep list/task indentation unchanged to avoid rendering regressions.

### Rename or Move Notes

- Update links that target the renamed or moved note.
- Preserve aliases in links unless asked to normalize them.
- Check both normal links and embeds for impacted paths or names.
- Search for unresolved references after the rename.

### Normalize Markdown for Obsidian

- Convert formats only when requested (for example, Markdown links to wikilinks).
- Preserve fenced code blocks and inline code during transformations.
- Keep callout, task, and table syntax valid in Obsidian preview mode.

## Validation Checklist

- Confirm YAML frontmatter still parses and remains the first block.
- Confirm no wikilink or embed was accidentally converted to plain text.
- Confirm renamed note references were updated.
- Confirm headings and block references used by links still exist.

## Reference File

Load [references/obsidian-patterns.md](references/obsidian-patterns.md) when exact syntax patterns or safe search/replace guidance is needed.
