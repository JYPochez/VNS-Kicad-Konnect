# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Konnect is an MCP server for KiCAD 10, written in Rust, shipped as a single binary that doubles as a KiCAD plugin. `DEV.md` is the long-form developer guide (full file tree, observability, error taxonomy); `tool-directory.md` is the generated catalog of every tool. Read those for detail — this file covers the parts that matter for making changes.

## Build & test

`protoc` and `cmake` are hard prerequisites (protobuf codegen in `konnect-ipc`; the `nng` crate compiles NNG's C library with cmake). macOS: `brew install protobuf cmake`.

```bash
cargo check --workspace                 # ~15s, the fast feedback loop
cargo test --workspace --lib --tests    # everything except schematic-viewer
cargo test -p konnect-sexp              # single crate
cargo test -p konnect-core router::     # single module's tests
cargo build --release -p konnect        # the MCP server binary
```

Pre-PR (matches `.github/workflows/ci.yml`):

```bash
cargo clippy --workspace -- -D warnings
cargo fmt --all
```

`crates/schematic-viewer` is deliberately **excluded from the workspace** (Tauri app) — `--workspace` never touches it and neither does CI. Build and test it explicitly:

```bash
cd crates/schematic-viewer && cargo build --release && cargo test
```

`crates/konnect-core/tests/conformance_test.rs` runs the parser against KiCAD's installed demo corpus and **skips silently** when no KiCAD is found. Set `KICAD_DEMOS=<path>` to force it to run — a green `cargo test` does not mean the conformance suite executed.

## Architecture

Five workspace crates, layered bottom-up:

- **`konnect-sexp`** — the S-expression engine. No KiCAD dependency. `parser.rs` (nom), `writer.rs` (`SexpEdit` + `apply_edits` + `write_atomic`), `geometry.rs` (**canonical** pin-transform math — don't reimplement pin rotation anywhere else).
- **`konnect-ipc`** — KiCAD 10 IPC client: NNG transport + protobuf (`build.rs` generates from `proto/`, copied from KiCAD v10 source). Synchronous/blocking.
- **`konnect-schematic-editor`** — typed schematic model (`Schematic`, `Symbol`, `Wire`, `Sheet`, `ChangeSet`) with lossless round-trips.
- **`konnect-core`** — all tool logic, the MCP protocol layer, the router, observability.
- **`konnect`** — binary + cdylib. CLI subcommands, config loading, stdio/HTTP transports, `ffi.rs` C ABI for the KiCAD plugin.

### Three backends, chosen per operation

This is the single most important thing to understand before editing a tool:

| Domain | Mechanism | Requires KiCAD running? |
|---|---|---|
| Schematic edits | direct `.kicad_sch` S-expression editing | no |
| PCB edits | IPC API (NNG + protobuf), undo-aware | **yes**, with the board open |
| Exports / ERC / DRC | `kicad-cli` subprocess (`tools/cli.rs`) | no |

`pcb_board.rs` tries IPC first and falls back to file editing, tagging the result `"source": "ipc"` vs file — preserve that pattern when adding board tools. IPC calls are blocking, so they're wrapped in `tokio::task::spawn_blocking` (see `with_ipc` in `pcb_board.rs` / `pcb_components.rs`). KiCAD 10's schematic IPC is **export-only** — there is no item CRUD, which is why schematic tools edit files directly.

### Two schematic representations, mid-migration

Both are live and the boundary is not clean:

- `konnect-sexp::schematic` — the original, still used by most of `sch_wiring` and nearly all of `sch_analysis` (the union-find net graph runs on these types).
- `konnect-schematic-editor` — the typed model, used by `sch_components`, `sch_hierarchy`, and the viewer.
- `tools/sch_bridge.rs` converts editor types → sexp types so the analysis code can consume them. It is explicitly a migration shim; when you migrate an analysis function to the typed model, delete the corresponding bridge function.

`tools/schematic_builder.rs` exists because **KiCAD 10's parser requires elements in a fixed order** (header → lib_symbols → junctions/no_connects → wires → text → labels → symbol instances *last*). Adding a new element kind to a schematic means routing it through `SchematicBuilder`, not appending to the file.

All file writes go through `write_atomic` (tmp → fsync → rename). fsync is mandatory, not defensive: tools read back immediately after writing.

### Tool routing (why `tools/list` is short)

Exposing all 185 tools costs ~25K tokens per listing, so the router only pre-loads `STARTER_KIT` (`project`, `config` — see `router/registry.rs`) plus 6 meta-tools; the model calls `load_toolset(name)` to expand. Consequences when changing tools:

- `router/registry.rs::ALL_TOOLSETS` carries a hand-maintained `tool_count` per toolset and `tools_for()` dispatches by name — both must be updated together with the toolset's `tools()` vec.
- Load/unload emits `tools/list_changed`.
- Calling an unloaded tool returns a structured error naming the owning toolset so the model can load and retry in one hop — don't degrade that message to a generic "unknown tool".

## Publishing to the fork

`./publish-to-fork.sh` (dry run) / `--push`. Never push without an explicit go-ahead.

The published tree must be **upstream's file set plus the code changes only**, so a PR reads
as a clean diff. These stay local and are stripped by the script:

| Withheld | Why |
|---|---|
| `CLAUDE.md` | agent instructions |
| `version_history.md` | local changelog; the README carries the public summary |
| `README-FORK.md` | superseded — the fork summary lives in `README.md` |
| `docs/CODE_REVIEW.md` | working notes |
| `docs/kicad-bug-report-*.md` | KiCad bug reports, not Konnect's concern |
| `publish-to-fork.sh` | this script |

`target/` and the local Rust toolchain are already gitignored. The script also lists any
other file not present in upstream `v0.2.2`, so nothing sneaks into a PR unnoticed, and runs
the CI gate (`fmt --check`, `clippy -D warnings`, tests) before pushing.

## Conventions

**Every commit updates `version_history.md`** — add an entry for the change under "Unreleased" with the problem, the fix and the tests, in the same commit as the code. The file is the fork's changelog and the source for `README-FORK.md`; a commit that changes behaviour without an entry there is incomplete.

**Adding a tool** — add a `tool!(name, desc, schema, handler)` entry to the toolset's `tools()` vec, write the `async fn handle_*` below it, bump `tool_count` in `router/registry.rs`, then regenerate the matching section of `tool-directory.md` (extraction procedure is in that file's header). The registry invariant tests in `router/mod.rs` catch a stale `tool_count`, duplicate tool names within and across toolsets, and any toolset exceeding 20 tools — but nothing checks `tool-directory.md`, so keep that in sync by hand.

**Errors** — failures are typed via `ToolErrorKind` in `mcp/error.rs` and serialized *inside* the text content (MCP's `CallToolResult` has no `data` field). Prefer `CallToolResult::error_kind(...)` over free-text `CallToolResult::error(...)`; migration from `anyhow` is incremental, so both exist. Adding a kind means editing the enum *and* `short_code()` — the `short_code_matches_serialized_kind_field` test fails if they drift.

**Arguments** — use `require_str` / `require_f64` / `opt_str` / `opt_f64` from `tools/mod.rs`. They emit structured `InvalidArgument` errors automatically; hand-rolled arg extraction loses that.

**Logging** — stdout is the MCP protocol channel. `tracing` writes to **stderr only**; never `println!` from tool code.

**File size** — several toolset files are already 40–80 KB (`library.rs` 82 KB, `sch_hierarchy.rs` 66 KB). Prefer adding a new module over growing these further.

## Bundled skills & agents

`crates/konnect/assets/skills/` and `assets/agents/` are compiled into the binary and written to `~/.claude/skills/` and `~/.claude/agents/` by `konnect init` (also run silently on first MCP launch). Editing those markdown files changes what ships to users — they are product surface, not repo docs.

CLI subcommands: `konnect init | uninstall | status | skill <name>`. Running the binary with stdin attached to a terminal triggers the friendly installer; piped stdin starts the MCP server.

## Config

`ServerConfig` (`tools/mod.rs`): `kicad_cli`, `kicad_binary`, `ipc_address`, `project_dir`, `jlcpcb_db_path`. Loaded from TOML/JSON with env fallbacks (`KICAD_API_SOCKET` is set by KiCAD when it launches the plugin — `apply_env_fallbacks()` must run on the `--config <path>` branch too). On macOS, KiCAD's tools live inside the app bundle and are not on PATH, so `kicad_cli` usually needs an explicit path.

## Known doc drift

`DEV.md` says "171 tools" in several places; the actual count is **185 across 18 toolsets** (+6 meta-tools), which matches `README.md` and the per-file `tool!` counts. Trust the source.
