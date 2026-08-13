# Version history — VNS-Kicad-Konnect

Changes made in this fork, newest first. Upstream is
[mixelpixx/Konnect](https://github.com/mixelpixx/Konnect); everything below sits on top of
upstream **v0.2.2** (2026-08-01).

Every entry was compiled and tested locally against Rust 1.96.0 (the version pinned in
`rust-toolchain.toml`) with `protoc` 3.20.3 and `cmake` 3.24.4 on macOS 15.7.7 / Apple Silicon.
The gate for each is upstream's own CI: `cargo test --workspace --lib --tests`,
`cargo clippy --workspace -- -D warnings`, `cargo fmt --all`.

---

## Unreleased — fixes on top of v0.2.2

Test count: **407 → 422** (15 added). Clippy clean on upstream's CI invocation.

### `146bb5b` — fix(mcp): contain a panicking tool handler instead of killing the server

**Problem.** Tool handlers were awaited inline in `dispatch_tool`, so a panic in any of the 187
tools unwound out of `handle_message`, out of the stdio read loop, and out of `main`. The MCP
client saw the transport die with no diagnostic, and every other loaded tool went with it.
`DEV.md` already claimed handler-panic was a structured error; it was not.

**Fix.** The handler now runs on a spawned tokio task, which converts a panic into a `JoinError`
instead of an unwind. The payload is reported as `ToolErrorKind::HandlerError` naming the tool.
tokio is already a direct dependency, so this adds no new crate. File writes are atomic
(tmp → fsync → rename), so an aborted call cannot leave a partial file behind.

This deliberately cannot catch a stack overflow, which aborts the process outright — that is why
`NetGraph::find` was made iterative in `3caab9d`.

**Tests.** Two: a panicking tool surfaces as a structured `handler_error`, and the server still
dispatches afterwards. Adds `ToolRouter::insert_tool_for_test`, which is `#[cfg(test)]` and does
not widen the public API.

### `ace68b7` — fix(sexp): decode string escapes in one pass

**Problem.** `unescape` chained four `replace` calls with the backslash collapse **last**, so an
already-escaped backslash was re-read as the introducer of the next escape:

| Serialized in file | Should decode to | Actually decoded to |
|---|---|---|
| `C:\\new\\temp` | `C:\new\temp` | `C:\` + newline + `ew\` + tab + `emp` |
| `a\\tb` | `a\tb` | `a\` + tab + `b` |

KiCad escapes backslashes in property values, so any Windows path whose next character was `n`,
`t` or `r` came back corrupted — `Datasheet`, 3D-model references, `Sheetfile`.

**Fix.** A single left-to-right scan. Unknown escapes now keep both characters rather than
dropping the backslash, so values round-trip.

**Blast radius.** Reads only. The parser is read-only and all writes go through targeted text
edits, so no file was ever damaged — the wrong string was simply handed to the caller.

**Tests.** Three: escaped backslashes, genuine control escapes, escaped quotes.

### `3caab9d` — fix(sch): make net queries deterministic and connect mid-wire points

Three defects in the union-find net graph, each with tests.

**1. `net_at` was nondeterministic.** It returned the first label reached while iterating
`point_nets`, a `HashMap`. Iteration order follows pointer hashing, so a net carrying more than
one label resolved to a different name between runs — the same file could report `VCC` on one run
and `+3V3` on the next. Roots are now collected, sorted and deduplicated. A new `nets_at` exposes
the full set, turning a silent coin-flip into a reportable label conflict.

**2. A query point on a wire's interior was reported unconnected.** Upstream v0.2.2 attaches
labels and junctions to segments when *building* the graph, but a pin landing mid-wire, or any
`trace_from_point` coordinate, is not known until it is asked about — it resolved to an isolated
component. `add_wire` now records its segment, and `attach_to_segments` runs on query as well as
on build. This also removed the duplicated attach closure.

**3. `find` recursed for path compression.** A long parent chain overflowed the stack, and a Rust
stack overflow aborts the process rather than raising a catchable panic. It is now a two-pass
iterative walk, and `union` attaches the smaller component under the larger so chains stay
shallow. Covered by a 50,000-segment test that overflowed before.

Also removed an `O(pins × labels)` full `HashMap` clone from `net_at`, which ran on every pin
during netlist export.

**Tests.** Ten, in `sch_analysis.rs::net_graph_tests`: shared endpoints, disjoint wires staying
separate, junction dots at a T, mid-segment labels, mid-wire pins, determinism across 50
rebuilds, conflict reporting, query stability, isolated points, and the 50k-segment stack test.

### `9eac6ad` — chore: update vendored source to upstream v0.2.2

Replaced the v0.2.0 tree with upstream v0.2.2, keeping the locally-added `CLAUDE.md` and `docs/`.
No files were removed upstream between the two versions, only added.

Two findings from `docs/CODE_REVIEW.md` were **fixed independently upstream** and needed no work
here:

- **HIGH-1** — mid-segment labels and junctions now feed the net graph (upstream #104). Upstream
  measured that **3,712 of 5,804 labels (64%)** in KiCad's own demo corpus sit mid-segment and
  were being dropped from net formation.
- **MED-1** — `write_atomic` scratch files are now per-process unique with a cleanup guard, so
  `.kicad_sch` and `.kicad_pcb` writes no longer collide on one temp path (upstream #71).

---

## 2026-08-10 — review and KiCad bug reports (no code change)

### `741f06a`, `42affbb` — KiCad bug reports

While probing KiCad 10's IPC API, eeschema crashed. Traced to a defect in KiCad itself, present in
**10.0.2 through 10.0.5**:

`API_HANDLER_SCH` passes no frame to `API_HANDLER_EDITOR` (the parameter defaults to `nullptr`)
and declares its own shadowing `m_frame`. `API_HANDLER_EDITOR::checkForBusy()` dereferences the
**base** pointer unguarded, so it is a null deref. `API_HANDLER_PCB` does it correctly.

Five commands crash eeschema: `HitTest`, `CreateItems`, `UpdateItems`, `DeleteItems`,
`BeginCommit`. Because `KICAD_API_SERVER` offers each request to every handler in a
`std::set<API_HANDLER*>` (pointer-ordered, effectively random per run), a *board* command can
reach the schematic handler and crash there — which appears to be the root cause of the open,
undiagnosed upstream issue KiCad #24966.

Two ready-to-send reports: `docs/kicad-bug-report-eeschema-api-null-frame.md` (GitLab template)
and `docs/kicad-bug-report-forum-post.md` (forum/Discord relay version). Neither is filed yet.

### `0b24390` — full code review of v0.2.0

`docs/CODE_REVIEW.md`. Four HIGH findings, five MEDIUM, four LOW. Two were subsequently fixed
upstream (above); the rest are addressed in this fork or still open — see the tracker below.

### `0460d2c` — initial commit

Upstream v0.2.0 imported into git. Excluded from version control: the unpacked 51 MB
`konnect-pcm-v0.2.0-macos` release package, `.DS_Store`, `__pycache__`, and a `.webloc` bookmark.
Added `CLAUDE.md` with build commands and architecture notes.

---

## Review findings still open

From `docs/CODE_REVIEW.md`, not yet addressed in this fork:

| ID | Finding | Notes |
|---|---|---|
| HIGH-4 | Nine toolsets have zero tests | `sch_analysis` now covered; `sch_batch`, `pcb_routing`, `verification`, `design_review`, `pcb_components`, `templates`, `manufacturing`, `sch_export` still bare |
| MED-3 | An unrelated `settings.json` in the working directory aborts startup | `Config::load` propagates the parse error instead of skipping the candidate |
| MED-4 | `find_toolset_for_tool` rebuilds every `ToolDef` on each call | Called once per tool call purely to fill an observability field; wants a `OnceLock` name→toolset index |
| LOW-2 | `parse_sexp` silently accepts trailing garbage | Returns a differently-shaped tree instead of an error, so a truncated file reads as "no symbols found" |
| LOW-3 | `resources/read` returns the `resources/list` response shape | Spec deviation, harmless today |
| LOW-4 | Twelve source files exceed 30 KB | `library.rs` is 82 KB |

## Local environment notes

Not part of the repository, recorded so the setup is reproducible:

- Konnect installed as a KiCad 10 plugin at
  `~/Documents/KiCad/10.0/3rdparty/plugins/com_github_mixelpixx_konnect`, upgraded 0.2.0 → 0.2.2.
  The v0.2.0 install is backed up alongside it.
- Registered as a user-scope MCP server so it is available in any directory.
- `settings.json` gained an explicit `kicad_binary` path — KiCad lives outside `/Applications`
  here, so auto-detection fails.
- KiCad project template `JY-Template` created under `~/kicad/template/`, with project-local
  symbol, footprint and design-block tables, plus a `KICAD_3D_JY` path variable.
