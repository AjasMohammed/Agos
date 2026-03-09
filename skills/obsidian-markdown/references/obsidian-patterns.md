# Obsidian Markdown Patterns

## Core Syntax

- Wikilink: `[[Note Name]]`
- Wikilink with alias: `[[Note Name|Visible Text]]`
- Heading link: `[[Note Name#Heading]]`
- Block link: `[[Note Name#^block-id]]`
- Embed note/section/block: `![[Note Name]]`, `![[Note#Heading]]`, `![[Note#^block-id]]`
- Tag: `#tag-name`
- Task item: `- [ ] open`, `- [x] done`
- Callout:

```md
> [!note] Title
> Callout body
```

## YAML Frontmatter

- Keep frontmatter as the first block in the file.
- Preserve key order unless the user asks to normalize it.
- Preserve unknown keys and list formatting.
- Keep date/time formatting consistent with nearby notes.

## Safe Edit Guidance

- Prefer minimal edits over full-file rewrites.
- Avoid changing line breaks in long list/task sections.
- Do not rewrite code fences unless the request targets code fence content.
- Preserve heading text that is likely link targets.

## Rename and Move Operations

- Update inbound references after filename/path changes.
- Check both links and embeds:
  - `[[Old Name]]`
  - `[[Old Name|Alias]]`
  - `[[Old Name#Heading]]`
  - `![[Old Name]]`
- Keep alias text unchanged unless instructed.

## Search Patterns

Use these patterns to inspect vault usage before bulk edits:

- Find wikilinks: `\[\[[^]]+\]\]`
- Find embeds: `!\[\[[^]]+\]\]`
- Find tasks: `^- \[[ xX]\]`
- Find callouts: `^> \[![^]]+\]`
- Find frontmatter block starts: `^---$`
