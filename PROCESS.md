# r2-rocker — Process discipline

We are pioneering not just the code but the way we develop it. This file
captures the working conventions for the project so future sessions (and
the eventual university handoff) inherit the same discipline.

Conventions adapted from the [`r2-specifications`][r2-specs] repo, which
treats specs as the source of truth.

## The four canonical artefacts

Every session produces output in one or more of these:

| Folder | Holds | Update cadence |
|---|---|---|
| `specifications/` | Source-of-truth spec documents — the contract that drives code | Spec changes precede code changes |
| `plan/` | Consolidated phasing, architecture decisions, open questions | Updated when phases close or decisions land |
| `conversation/` | Verbatim (or faithful) record of design sessions, one file per session | Append-only; one new file per session |
| `docs/` | External references (datasheets, links to vendor docs) | Adds only; original sources |

Code lives in `firmware/`, `dashboard/`, `tools/`, `crates/`. Build
artefacts (`target/`, `build/`) are gitignored.

## The five rules

### 1. Spec before code

No firmware/dashboard code is merged before its driving specification
exists in `specifications/`. The spec captures *what* and *why*; the code
implements *how*.

When the spec and code disagree, the spec wins by default — update the
code or, if the code revealed a real-world constraint, update the spec
*and document why* in the spec's change log.

### 2. Conversation is research data

Every design session is archived in `conversation/` as a markdown file
named `YYYY-MM-DD-<topic>-NN.md`. The archive captures:

* User messages **verbatim**.
* AI responses faithfully — full technical content (tables, equations,
  citations), prose density may be reduced.
* The **decisions** table at the end of the file — bullet list of every
  binding choice made in that session, with a reference back to the
  exchange that produced it.
* **Open questions** still pending after the session.
* **Next session entry points** so the next session knows where to pick up.

The archive is **append-only** — never edit a closed session retroactively.
If a decision was wrong, capture the correction in a *new* session and
mark the prior decision as superseded.

### 3. Plan is the single point of consolidation

`plan/PLAN.md` is the place to look for "what's the current state of the
project?" — phasing with completion status, architecture summary, deferred
work. It overwrites itself; conversation archives accumulate.

When in doubt about whether to put something in plan vs. conversation: if
it's a decision someone might want to look up six months from now, plan.
If it's the rationale behind that decision, conversation.

### 4. Secrets stay out

`specifications/SECRETS-POLICY.md` is binding. The repo never receives
private key material, WiFi credentials, NVS dumps, or per-device secrets.
The `.gitignore` is the second line of defence; the first is *don't put
secrets in the working tree in the first place*.

Before any push, run a quick scanner:
```bash
gitleaks detect --source . --no-banner
```

### 5. Cite your sources

When a decision references a datasheet, vendor doc, or external paper,
cite the file path + page (or URL + section) in the spec. Future
maintainers — including the university — must be able to reconstruct the
reasoning without our presence.

## File-naming conventions

| Kind | Pattern | Example |
|---|---|---|
| Specification | `SPEC-<scope>.md` or descriptive title | `HARDWARE-WIRING.md`, `SPEC-R2-ROCKER-SENSOR.md` |
| Plan | `PLAN.md` (single root file) | `plan/PLAN.md` |
| Conversation archive | `YYYY-MM-DD-<topic>-NN.md` | `2026-05-06-design-session-01.md` |
| Decision record (if needed) | `DEC-NNNN-<title>.md` (Architecture Decision Record) | `DEC-0001-hardwired-trust-group.md` |

We're not heavy on ADRs yet — the conversation archives do most of the
work. Promote a decision to a separate ADR when it has wide-reaching
consequences and you want it discoverable in isolation.

## Versioning specs

Each spec has a frontmatter block:

```yaml
---
title: <short title>
status: Draft v0.X | Review | Frozen vY.Z
date: YYYY-MM-DD
applies-to: <hardware / scope>
---
```

`status` progresses Draft → Review → Frozen. Once frozen, only patch-level
edits (typo fixes, clarifications) are allowed; substantive changes
require a new version with the prior version archived under the spec's
own `change log`.

## Checklist before ending a session

- [ ] New conversation file in `conversation/` for this session.
- [ ] Decisions table appended at the bottom of the conversation file.
- [ ] `plan/PLAN.md` updated if any phase changed status or any binding
      decision was made.
- [ ] Any new secret-bearing file gitignored *before* it's saved.
- [ ] Tasks marked complete or carried forward.

[r2-specs]: https://github.com/reality2-ai/r2-specifications
