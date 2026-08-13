# VNS-Kicad-Konnect

A fork of [mixelpixx/Konnect](https://github.com/mixelpixx/Konnect) — the Rust MCP server that
lets Claude and other AI assistants design KiCAD 10 schematics and PCBs.

This fork exists to fix correctness bugs found during a full read of the codebase. It tracks
upstream closely and is intended to feed changes **back** to upstream rather than diverge. Every
change here is a candidate pull request.

Base: upstream **v0.2.2** (2026-08-01). See [`version_history.md`](version_history.md) for the
detailed log and [`docs/CODE_REVIEW.md`](docs/CODE_REVIEW.md) for the review that started it.

---

## Why this fork exists

Konnect is well-built — the architecture is sound, the KiCAD-specific knowledge in
`geometry.rs` is genuinely hard-won, and the tool router is a good idea well executed. But it is
young, and a full review turned up a cluster of correctness bugs concentrated in exactly the place
a young project would have them: **the connectivity engine had the least test coverage and the
most risk**.

For a tool whose job is producing manufacturable boards, a netlist that disagrees with the
schematic is the most expensive possible bug. That is what these fixes are about.

Upstream reached two of the same conclusions independently in v0.2.1/v0.2.2, which is a good sign
about the direction of the project — those are noted below rather than claimed here.

## What this fork changes

### Net connectivity is now deterministic and complete

Three separate defects in the union-find net graph:

- **`net_at` returned a different answer between runs.** It took the first label reached while
  iterating a `HashMap`, and iteration order follows pointer hashing. A net carrying two labels
  could report `VCC` on one run and `+3V3` on the next — for the same file. Results are now
  sorted and stable, and a new `nets_at` returns the whole set, so a conflicting label becomes a
  *reportable design error* instead of a silent coin-flip.

- **A pin on a wire's interior read as unconnected.** Upstream v0.2.2 attaches labels and
  junctions to segments when building the graph, but a query point — a pin landing mid-wire, or
  any `trace_from_point` coordinate — is not known until it is asked about, and resolved to an
  isolated component. Query points are now attached too.

- **`find` could abort the process.** Path compression recursed, so a long parent chain overflowed
  the stack — and a Rust stack overflow aborts outright; no handler can catch it. It is now
  iterative, with union-by-size so chains stay shallow. There is a 50,000-segment test that
  overflowed before this change.

### A panicking tool no longer kills the server

Handlers were awaited inline, so a panic in any of the 187 tools unwound out of the stdio loop and
out of `main`. The client saw the transport die with no diagnostic and lost every other loaded
tool. Handlers now run on a spawned task, so a panic becomes one failed call reported as a
structured `handler_error`.

### String escapes decode correctly

`unescape` chained `replace` calls with the backslash collapse last, so an already-escaped
backslash was re-read as the start of the next escape. A serialized `C:\\new` — the literal path
`C:\new` — came back as `C:\` followed by a real newline. KiCAD escapes backslashes in property
values, so Windows paths in `Datasheet`, 3D-model references and `Sheetfile` decoded wrongly.

Reads only: the parser is read-only and writes go through targeted text edits, so no file was ever
damaged — the caller was simply handed the wrong string.

### Tests

**407 → 422.** `sch_analysis.rs` had no test module at all despite holding the most
algorithmically dense code in the project; it now has ten covering shared endpoints, disjoint
wires, junction dots, mid-segment labels, mid-wire pins, determinism across repeated rebuilds,
conflict reporting, and the stack-overflow case.

## Fixed upstream, not here

Two findings from the review were resolved independently in upstream v0.2.1/v0.2.2 before this
fork was cut. Credit where due:

- **Mid-segment labels and junctions feed the net graph** (upstream #104, by @Nigh). Upstream
  measured that **3,712 of 5,804 labels (64%)** in KiCAD's own demo corpus sit mid-segment and
  were being dropped from net formation entirely.
- **Atomic-write scratch files are per-process unique** with a cleanup guard (upstream #71), so a
  `.kicad_sch` and a `.kicad_pcb` sharing a filename stem no longer collide on one temp path.

## Still open

Tracked in [`version_history.md`](version_history.md): eight toolsets still have no tests; an
unrelated `settings.json` in the working directory aborts startup; `find_toolset_for_tool`
rebuilds every tool definition on each call; `parse_sexp` accepts trailing garbage silently.

## A KiCAD bug found along the way

Not a Konnect issue, but it affects anyone using the IPC API. `API_HANDLER_SCH` passes no frame to
`API_HANDLER_EDITOR` and declares a shadowing `m_frame`, so `checkForBusy()` dereferences null.
**Five IPC commands crash eeschema on KiCAD 10.0.2 through 10.0.5**: `HitTest`, `CreateItems`,
`UpdateItems`, `DeleteItems`, `BeginCommit`.

Because `KICAD_API_SERVER` offers each request to every registered handler in a
`std::set<API_HANDLER*>` — ordered by pointer value, so effectively random per run — a *board*
command can reach the schematic handler and crash there. That appears to be the root cause of the
open, undiagnosed KiCAD issue #24966.

Practical consequence for Konnect users: **close the Schematic Editor before running PCB tools.**
Reports ready to file are in [`docs/`](docs/).

## Building

`protoc` and `cmake` are hard prerequisites (protobuf codegen; the `nng` crate builds NNG's C
library with cmake). Rust is pinned to 1.96.0 by `rust-toolchain.toml`.

```bash
brew install protobuf cmake          # macOS
cargo test --workspace --lib --tests
cargo clippy --workspace -- -D warnings
cargo build --release -p konnect
```

`crates/schematic-viewer` is excluded from the workspace and built separately.

## Relationship to upstream

Upstream remains the home of the project. This fork carries fixes only, and they are offered back
as pull requests. Upstream's contributor agreement asks that contributions be relicensable, which
applies to anything sent from here.

## License

**AGPL-3.0-only**, unchanged from upstream. See [LICENSE](LICENSE) and
[COMMERCIAL.md](COMMERCIAL.md).
