---
name: mcp-apps-sync-docs
description: >
  Syncs MCP Apps documentation with the @apollo/client-ai-apps changelog. Always use this skill
  whenever: (1) the user wants to update or sync the mcp-apps-*.mdx docs based on a new release
  or version range, (2) phrases like "sync docs from version X", "update docs from changelog",
  "what changed since v0.x.x", "docs are out of date", or "check the changelog" appear in context
  of MCP Apps documentation, (3) checking whether mcp-apps-*.mdx files reflect recent releases.
  The changelog URL and doc file paths are pre-configured — the user only needs to provide the
  minimum version to sync from.
---

## Overview

This skill updates the MCP Apps documentation based on changes in the `@apollo/client-ai-apps` CHANGELOG from a specified version onward. It reads the changelog, compares it against the current docs, and makes targeted edits — without committing.

## What you need from the user

One thing: **a minimum version** (e.g., `0.6.0` or `v0.6.2`). All changelog entries from that version to the latest will be analyzed.

## Step 1: Fetch the changelog

Use the GitHub CLI to fetch the full changelog:

```bash
gh api repos/apollographql/apollo-client-ai-apps/contents/CHANGELOG.md --jq '.content' | base64 -d
```

## Step 2: Parse relevant versions

The changelog uses this structure:

```
## 0.6.5 (2026-03-23)

### Features

#### Feature title

Description and code examples...

### Fixes

#### Fix title

Description...
```

Collect all version sections where the version number is **greater than or equal to** the user's minimum version. Ignore everything below that version. Parse each section into: version number, date, list of features, list of fixes, and any breaking changes or deprecations called out explicitly.

## Step 3: Read the docs

Read all five documentation files. Understand what each one covers before analyzing the changelog:

- `docs/source/mcp-apps-architecture.mdx` — How MCP Apps work: the discovery and tool execution phases, architecture diagrams
- `docs/source/mcp-apps-quickstart.mdx` — Step-by-step setup guide using the Apollo AI Apps Template; Vite plugin behavior; project structure
- `docs/source/mcp-apps-development.mdx` — Day-to-day development patterns, directives, hooks, and client usage
- `docs/source/mcp-apps-prerequisites.mdx` — System and account prerequisites before building
- `docs/source/mcp-apps-reference.mdx` — API reference: directives (`@tool`, `@prefetch`, `@private`), hooks (`useToolInfo`, etc.), manifest, configuration

## Step 4: Analyze the changes

For each changelog entry, decide whether a doc change is needed:

**Features** — Is this already documented? If not, which file is the right home?

- New hooks, directives, or client APIs belong in `mcp-apps-reference.mdx`
- Changes to how Vite plugin builds or generates files belong in `mcp-apps-quickstart.mdx` or `mcp-apps-development.mdx`
- Changes to how the architecture works belong in `mcp-apps-architecture.mdx`
- New system requirements belong in `mcp-apps-prerequisites.mdx`

**Deprecations** — Are the deprecated APIs mentioned positively in the docs? Add a deprecation notice near the relevant section and point to the replacement.

**Breaking changes / behavior changes** — Do the docs describe the old behavior as correct? Update them to reflect the new behavior.

**Bug fixes** — Generally skip these unless the docs explicitly described the buggy behavior as correct (e.g., documented a workaround that is no longer needed).

## Step 5: Make the edits

Edit the affected files directly using available tools. As you do:

- Match the existing tone, MDX component usage (e.g., `<Note>`, code fences with language tags), and heading levels
- Insert new content in the section that makes the most sense contextually, not just at the end of the file
- For deprecations, add a notice near the existing mention — don't remove the old API from docs until it's actually gone
- For breaking changes, update the description in place so readers see the current behavior
- Keep each change minimal and traceable to its changelog entry
- Write as if the behavior or code was always that way. DO NOT write in past tense (avoid language such as "previously it was x, now it is y")

**Do not commit.** Leave all edits as unstaged file changes for the user to review.

## Step 6: Summarize what you did

Give the user a concise report:

- Which files were modified
- For each change: what was updated and which changelog entry it corresponds to
- Which changelog entries were skipped and why (e.g., "internal fix, no user-facing behavior change")
