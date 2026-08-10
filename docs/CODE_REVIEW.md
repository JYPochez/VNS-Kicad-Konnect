# Code Review — Konnect v0.2.0

**Date:** 2026-08-10
**Reviewer:** Claude Opus 5 (Claude Code)
**Commit:** `0460d2c` (initial import)
**Scope:** full workspace — `konnect`, `konnect-core`, `konnect-sexp`, `konnect-ipc`, `konnect-schematic-editor`, plus `schematic-viewer`, `plugin/`, `packaging/`, CI.

## Method & limitations

Read-only review of ~58k lines. **No Rust toolchain is installed on this machine**, so nothing here was validated by `cargo check`, `cargo test`, or `cargo clippy` — every finding comes from reading the source. Where a claim is about pure string or path logic, I re-implemented the exact function in another language and ran it; those are marked **verified by simulation**. Everything else is marked **by inspection** and should be confirmed with a test before you act on it.

I did not review: the generated protobuf in `konnect-ipc/src/gen.rs`, the SVG assets, or `LICENSE`.

---

## Overall assessment

This is **well-built software**. It is markedly more disciplined than most code in the AI-tooling space, and the engineering judgment behind the big decisions is sound.

What stands out:

- **The architecture earns its rewrite.** The README's case against the Python/TypeScript predecessor — four serialization boundaries, a dead-end SWIG dependency, a huge install surface — is accurate, and the Rust design actually fixes those problems rather than relocating them.
- **The hard-won knowledge is captured where it belongs.** `geometry.rs` is the standout: KiCAD's Y-up/Y-down flip, the screen-CCW rotation matrix, and mirror-after-rotation are documented against eeschema's own `symbol.h` semantics, with tests carrying the ground-truth values and a test named for the exact ordering bug the predecessor shipped. That is institutional memory encoded as executable spec.
- **Comments explain *why*, not *what*.** `handler.rs:24-28` explains that stdio notification sinks exist because issue #19 silently dropped `tools/list_changed`. `writer.rs:196-197` explains that fixed-width indentation literals fail because eeschema writes tabs while this crate writes spaces. These are the comments that survive contact with a future maintainer.
- **The router is a genuinely good idea**, well-executed, and its invariants are enforced by tests (`router/mod.rs:178-263`) rather than by hope — stale `tool_count`, duplicate names across toolsets, and oversized toolsets all fail the build.
- **Adversarial tests where they matter.** `config.rs:209-241` throws garbage TOML, wrong-typed JSON, and non-object roots at the loader and asserts `Err`, not panic. The conformance suite (`conformance_test.rs`) uses KiCAD's own demo corpus as an oracle — the right instinct.

The concerns below are real, but they sit on a solid foundation. Nothing here suggests rearchitecting.

**Beta status is accurate.** The gaps are concentrated in exactly the place a young project would have them: the connectivity engine has the least test coverage and the most correctness risk.

---

## Findings

Ordered by severity. Each has a location, why it matters, and a suggested fix.

### HIGH-1 — Net connectivity ignores T-junctions and mid-wire attachment

**`crates/konnect-core/src/tools/sch_analysis.rs:219-275`** — by inspection

`build_net_graph` unions only the two **endpoints** of each wire:

```rust
pub(crate) fn add_wire(&mut self, w: &Wire) {
    let a = pt_key(w.x1, w.y1);
    let b = pt_key(w.x2, w.y2);
    self.ensure(a); self.ensure(b); self.union(a, b);
}
```

Two electrically-connected situations are therefore invisible to the graph:

1. **T-junction** — wire A runs (0,0)→(10,0), wire B runs (5,0)→(5,10). KiCAD treats these as one net (with a junction dot). The graph puts them in two disjoint sets, because (5,0) is not an endpoint of A.
2. **Pin or label on a wire's interior** — `net_at()` calls `ensure(k)`, which creates an isolated singleton node for that coordinate, then finds nothing. Returns `None` for a pin that is actually connected.

This is not a missing utility — `point_on_segment` exists in `geometry.rs:105` and is explicitly documented "Used for T-junction detection." It is applied **ad hoc in three handlers** (`sch_export.rs:369`, `sch_batch.rs:840`, `sch_analysis.rs:556`) but **never in the shared graph builder** every connectivity query runs through.

Blast radius — every consumer of `net_at`:

| Call site | Tool |
|---|---|
| `sch_analysis.rs:441` | `get_pin_net` |
| `sch_analysis.rs:474` | `get_component_nets` |
| `sch_analysis.rs:566` | point query |
| `sch_analysis.rs:695` | net listing |
| `sch_batch.rs:934` | batch connectivity check |
| `sch_export.rs:223` | **netlist export** |

The failure is silent and it under-reports: the LLM is told a pin is unconnected when it is connected, and a netlist export can omit real connections. For a tool whose job is producing manufacturable boards, a wrong netlist is the most expensive possible bug.

**Fix:** fold T-junction resolution into `build_net_graph` — after adding all wire endpoints, for every node key, test it against every wire segment with `point_on_segment` and union on a hit. Naively that is O(nodes × wires); bucket by coordinate if it matters. Also union explicit `(junction …)` elements from the file, which the graph currently ignores entirely.

**This is the single highest-value fix in the codebase**, and it needs tests — see HIGH-4.

### HIGH-2 — `net_at` returns a nondeterministic net name

**`crates/konnect-core/src/tools/sch_analysis.rs:233-244`** — by inspection

```rust
let labels: Vec<_> = self.point_nets.clone().into_iter().collect();
for (lk, net) in labels {
    if self.find(lk) == root { return Some(net); }
}
```

`point_nets` is a `HashMap`. Iteration order is unspecified and, with Rust's default `RandomState`, **differs between process runs**. When one net carries more than one label — two labels on the same net is ordinary practice, and a mislabeled net is exactly what a design review should catch — the returned name is whichever the iterator happened to reach first.

Consequences: the same schematic can yield different `get_pin_net` answers on different runs; results are unreproducible; and a genuine label conflict is silently resolved instead of reported.

**Fix:** collect all matching net names, not the first. Return them sorted, and treat length > 1 as a diagnostic the caller can surface ("net has conflicting labels: VCC, +3V3"). That converts a nondeterminism bug into a design-review feature.

### HIGH-3 — No panic boundary: one panicking handler kills the server

**`crates/konnect-core/src/mcp/handler.rs:230`**, **`crates/konnect/src/transport/stdio.rs:48`** — by inspection

The tool handler is awaited directly:

```rust
return match (tool_def.handler)(args, self.ctx.clone()).await {
```

and the stdio loop awaits `handle_message` directly. There is no `catch_unwind`, no `tokio::spawn` isolating the call. A panic in any of the 185 handlers unwinds through `run_stdio` and out of `main` — the process dies, and the MCP client sees the connection drop with no error message.

There are live panic sources reachable from tool arguments:

- `konnect-sexp/src/writer.rs:80-81` — `apply_edits` uses `assert!` for out-of-bounds and inverted edit ranges. A handler that miscomputes an offset panics rather than returning an error.
- `konnect-core/src/tools/cli.rs` — 36 `.to_str().unwrap()` calls on paths (lines 97, 100, 172, 179, 283-284, 298-299, 314-315, 346, 349, …). A non-UTF-8 path — legal on macOS and Linux — panics.

`DEV.md:175` states that "the dispatch-level errors (not-loaded/unknown/**handler-panic**) are fully structured." The not-loaded and unknown paths are; **the handler-panic path does not exist.** The doc should not claim a safety property the code doesn't have.

**Fix (two parts):**
1. Wrap the handler call in `AssertUnwindSafe(...).catch_unwind()` and map a panic to `ToolErrorKind::HandlerError`. That makes the DEV.md claim true and turns a server death into one failed tool call.
2. Change `apply_edits` to return `Result<String, SexpError>` instead of asserting. It's a library function on a hot path for file mutation — a bad offset should be a typed error, not an abort.

### HIGH-4 — Nine toolsets have zero tests, including the connectivity engine

By inspection (test counts exclude `#[cfg(test)]` blocks in other files)

| Toolset | Prod lines | In-file tests |
|---|---|---|
| `sch_analysis.rs` | 819 | **0** |
| `sch_batch.rs` | 978 | **0** |
| `pcb_routing.rs` | 801 | **0** |
| `verification.rs` | 868 | **0** |
| `design_review.rs` | 1071 | **0** |
| `pcb_components.rs` | 661 | **0** |
| `templates.rs` | 589 | **0** |
| `manufacturing.rs` | 544 | **0** |
| `sch_export.rs` | 433 | **0** |

The workspace has ~265 tests overall and the low-level crates are well covered — `konnect-sexp` (40), `konnect-schematic-editor` (34). The gap is specifically in the tool layer, and it correlates exactly with where HIGH-1 and HIGH-2 live: `sch_analysis.rs` holds the union-find graph, the most algorithmically dense code in the project, and has no unit tests at all.

Some of these are defensible — `pcb_components` and `pcb_routing` need a running KiCAD, and thin `kicad-cli` wrappers are reasonably left untested (a convention DEV.md states explicitly). But `sch_analysis`, `sch_batch`, and `design_review` are pure functions over parsed data with no external dependency. They are testable today.

**Fix:** start with a table-driven test for `build_net_graph` covering: two wires sharing an endpoint; a T-junction; a pin mid-segment; two disjoint nets; a net with two labels. Those five cases would have caught both HIGH-1 and HIGH-2.

### MED-1 — `write_atomic` temp path collides between schematic and board

**`crates/konnect-sexp/src/writer.rs:97`** — verified by simulation

```rust
let tmp_path = path.with_extension("kicad_tmp");
```

`with_extension` *replaces* the extension. In a KiCAD project the schematic and board share a filename stem:

| Target | Temp path |
|---|---|
| `board.kicad_sch` | `board.kicad_tmp` |
| `board.kicad_pcb` | `board.kicad_tmp` ← **same file** |

Two concurrent writes race on one temp path, and the `rename` can move the wrong content over the target — silent cross-contamination between a schematic and a board.

Is concurrency reachable? On stdio, requests are handled serially, so no. On HTTP it is: `http.rs:92` `handle_post` has no serialization, so axum serves concurrent POSTs in parallel. Two Claude sessions pointed at one project also collide.

Two smaller issues in the same function:
- **The temp file leaks on failure.** If `write_all` or `sync_all` errors, the `.kicad_tmp` file is left on disk with no cleanup guard.
- **`.gitignore` doesn't catch it.** The pattern is `*.tmp`, which does not match `board.kicad_tmp` (the extension is `kicad_tmp`). Leaked temp files show up as untracked noise in the user's project.

**Fix:** build the temp name from the full filename plus a unique suffix — `board.kicad_sch.<pid>.<counter>.tmp` — which is collision-free and matches `*.tmp`. Wrap cleanup in a guard so a failed write removes the temp file.

### MED-2 — `unescape` corrupts escaped backslashes

**`crates/konnect-sexp/src/parser.rs:156-161`** — **verified by simulation**

```rust
fn unescape(s: &str) -> String {
    s.replace("\\\"", "\"")
        .replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\\\", "\\")
}
```

Sequential `replace` is not a valid unescaper: the backslash-collapse runs **last**, so an escaped backslash followed by `n` or `t` is misread as a control-character escape.

Verified results:

| Serialized in file | Should decode to | Actually decodes to |
|---|---|---|
| `C:\\new\\temp` | `C:\new\temp` | `C:\` + newline + `ew\` + tab + `emp` |
| `a\\tb` | `a\tb` (literal) | `a\` + tab + `b` |

KiCAD escapes backslashes in quoted strings, so any property holding a Windows path — `Datasheet`, a 3D-model reference, a `Sheetfile` — decodes wrong whenever the following character is `n` or `t`. `C:\new\...` and `C:\temp\...` are not exotic paths.

Blast radius is contained: the parser is read-only and all writes are text edits, so **files are not corrupted** — the LLM is just handed a wrong string. Good architectural luck, not a reason to leave it.

**Fix:** single left-to-right pass over the characters, consuming `\` plus the next character as one unit. ~10 lines, and the two rows above become the test cases.

### MED-3 — An unrelated `settings.json` in the working directory kills startup

**`crates/konnect/src/config.rs:82-95`** — by inspection

```rust
let mut config_paths = vec![
    PathBuf::from("konnect.toml"),
    PathBuf::from("settings.json"),
];
...
for path in &config_paths {
    if path.exists() {
        config = Some(Self::load_from(path)?);   // <-- `?`
        break;
    }
}
```

Both entries are **relative to the current working directory**, which for an MCP server is whatever the launching client chose — often the user's project directory. `settings.json` is one of the most common filenames in software (VS Code, countless JS tools). If one exists in CWD, Konnect parses it as its own config, and the `?` makes a parse failure a **hard startup abort**, not a skip.

The user-visible symptom is the worst kind: Konnect works everywhere except in one project directory, with a failure that looks unrelated to the file that caused it.

**Fix:** on a parse error, `warn!` and continue to the next candidate path rather than propagating. Separately, consider dropping bare `settings.json` from the CWD search — it's too generic a name to claim; `konnect.toml` and the exe-relative/platform paths already cover the real cases.

### MED-4 — Every tool call rebuilds all 185 tool definitions

**`crates/konnect-core/src/router/mod.rs:65-74`**, called from **`handler.rs:163-167`** — by inspection

```rust
pub fn find_toolset_for_tool(&self, tool_name: &str) -> Option<&'static str> {
    for ts in self.registry {
        if let Some(defs) = registry::tools_for(ts.name) {   // <-- constructs the whole Vec
            if defs.iter().any(|d| d.name == tool_name) { return Some(ts.name); }
```

`tools_for()` calls the toolset's `tools()`, which allocates a `Vec<ToolDef>` — each entry building a `json!` schema (nested maps, string allocations) and an `Arc`'d closure. `find_toolset_for_tool` scans toolsets until it matches, so a tool in a late toolset constructs most of the 185 definitions. `execute_tool` calls it **unconditionally on every tool call**, just to fill in the observability record's `toolset` field — and the error path calls it a second time.

Nothing breaks; it's pure waste on the hot path, and it will get worse as the tool count grows.

**Fix:** build a `&'static str → &'static str` name→toolset index once in a `OnceLock` and look up in O(1). The registry-invariant tests already guarantee names are unique, so the index is well-defined.

### MED-5 — `net_at` clones the label map on every call

**`crates/konnect-core/src/tools/sch_analysis.rs:237`** — by inspection

```rust
let labels: Vec<_> = self.point_nets.clone().into_iter().collect();
```

A full `HashMap` clone plus a `Vec` allocation per call, and `net_at` is called once per pin in `get_component_nets` (`sch_analysis.rs:474`) and once per pin during netlist export (`sch_export.rs:223`). That is O(pins × labels) clones of `String` keys and values on a path that runs over an entire schematic.

The clone exists only to dodge a borrow conflict with `&mut self.find()`.

**Fix:** collect the keys into a `Vec<(i64,i64)>` first (cheap, `Copy`) and look up the name after resolving roots — the same technique already used correctly in `points_on_net` at line 248, with a comment explaining exactly this. Applying that pattern here removes the clone.

### LOW-1 — Recursive union-find can overflow the stack

**`crates/konnect-core/src/tools/sch_analysis.rs:200-217`** — by inspection

`find` recurses for path compression, and `union` has **no union-by-rank or size** — it always attaches `rb` under `ra`. Adversarial or merely unlucky wire ordering builds a long chain, and the first `find` on it recurses to chain depth. A Rust stack overflow is an immediate `SIGSEGV`/abort that no `catch_unwind` can trap, so this is a hard process kill, not a failed tool call.

A large hierarchical schematic has thousands of wire segments. The trigger needs a degenerate ordering, so this is unlikely rather than impossible.

**Fix:** iterative two-pass `find` (walk to root, then re-walk setting parents), plus union-by-size. Standard, ~15 lines, removes the failure mode entirely.

### LOW-2 — `parse_sexp` silently accepts trailing garbage

**`crates/konnect-sexp/src/parser.rs:92-111`** — by inspection

When input remains after the first complete expression, the parser wraps everything in an implicit `List` and returns `Ok`. Inside that loop, `Err(_) => break` **discards the remainder without reporting it**.

So a truncated or corrupt `.kicad_sch` doesn't produce a parse error — it produces a successfully-parsed tree with a *different shape* (an extra list level), and every `find("kicad_sch")` against it silently returns `None`. The caller then reports "no symbols found" rather than "this file is corrupt."

Legitimate KiCAD files have exactly one top-level expression, so the multi-node path only ever fires on malformed input.

**Fix:** return `Err(SexpError::Parse)` with the byte offset when non-whitespace trailing input remains. If some caller genuinely needs multi-expression parsing, give it a separate `parse_sexp_multi`. Also note `SexpError::Parse { offset: 0 }` at line 114 hardcodes offset 0, discarding nom's actual error position — worth threading through, since these are the errors a user will need to debug.

### LOW-3 — `resources/read` returns the wrong response shape

**`crates/konnect-core/src/mcp/handler.rs:146`** — by inspection

```rust
"resources/list" | "resources/read" => Ok(Some(json!({ "resources": [] }))),
```

Per MCP, `resources/read` returns `{ "contents": [...] }`, not `{ "resources": [] }`. Harmless today since the server advertises no resources and no client will call it, but it's a spec deviation sitting in a match arm that looks deliberate.

**Fix:** split the arms, or return a proper "method not found" for `resources/read`.

### LOW-4 — Twelve source files exceed your 30 KB refactor threshold

By inspection. Against your standing rule ("if a source file is more than 30 KB then I refactor it into 2 or more smaller files"):

| File | Size |
|---|---|
| `konnect-core/src/tools/library.rs` | 82 KB |
| `konnect-core/src/tools/sch_hierarchy.rs` | 66 KB |
| `konnect-core/src/tools/sch_wiring.rs` | 60 KB |
| `schematic-viewer/src/main.rs` | 50 KB |
| `konnect-core/src/tools/sch_components.rs` | 47 KB |
| `konnect-core/src/tools/integration.rs` | 40 KB |
| `konnect-core/src/tools/design_review.rs` | 39 KB |
| `konnect-core/src/tools/sch_batch.rs` | 36 KB |
| `konnect-core/src/tools/pcb_board.rs` | 36 KB |
| `konnect-core/src/tools/verification.rs` | 32 KB |
| `konnect-ipc/src/client.rs` | 30 KB |
| `konnect-core/src/tools/sch_analysis.rs` | 30 KB |

`library.rs` at 82 KB is nearly triple the threshold. The natural split for a toolset file is `<name>/mod.rs` (the `tools()` vec) + `<name>/handlers_*.rs` grouped by theme — symbol vs. footprint operations in `library.rs`, sheet CRUD vs. pin lifecycle in `sch_hierarchy.rs`.

On your **300-line function** rule, the codebase is in good shape: only two functions exceed it — `sch_wiring.rs:26 fn tools` (327) and `library.rs:15 fn tools` (310) — and both are flat declarative `vec![]` literals, not logic. I'd leave them; splitting them into a *file* is the useful move, not splitting the function.

### NIT — Documentation drift

- `DEV.md` says "171 tools" in three places (lines 196, and the header claim). The real count is **185 across 18 toolsets** (+6 meta-tools), matching `README.md` and `DEV.md:251`'s own stats section. The `.webloc` bookmark in the project root repeated the stale 171 too.
- `DEV.md:32-107` file tree omits `tools/sch_bridge.rs`, `tools/schematic_builder.rs`, and `konnect/src/{install,manifest}.rs`.
- `DEV.md:175` overstates panic safety — see HIGH-3.

---

## Recommended order

1. **HIGH-4 first, narrowly** — write the five `build_net_graph` cases. They fail, and they define done for the next two items.
2. **HIGH-1** — fold T-junctions and junction elements into the graph builder.
3. **HIGH-2** — make `net_at` deterministic; return all labels, surface conflicts.
4. **HIGH-3** — `catch_unwind` in the dispatcher, then make `apply_edits` return `Result`.
5. **MED-1, MED-2, MED-3** — small, independent, each a handful of lines with an obvious test.
6. **MED-4, MED-5** — mechanical performance fixes, no behavior change.
7. **LOW-1** — iterative find + union by size.
8. **LOW-4** — split files as you next touch them, rather than as a big-bang refactor.

Items 1-3 are one focused session and they retire the correctness risk that matters most: a netlist that doesn't match the schematic.

---

## Things I deliberately did not flag

- **Tools accept arbitrary filesystem paths.** Inherent to a local design tool; the HTTP transport binds to `127.0.0.1` and validates `Origin` (`http.rs:71-90`), which is the right mitigation. Worth revisiting only if remote binding is ever supported.
- **`.to_str().unwrap()` in `cli.rs`** is listed under HIGH-3 as a panic source, but the underlying non-UTF-8-path handling isn't worth fixing on its own once a panic boundary exists.
- **`schematic-viewer` untested subprocess/Tauri plumbing** — DEV.md states this convention explicitly and the 20 tests cover the pure logic. That's a reasonable line.
