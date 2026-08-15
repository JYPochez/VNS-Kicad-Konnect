<a name="top"></a>

<div align="center">

<img src="resources/images/KiCAD-MCP-Server-rust.svg" alt="KiCAD-MCP-Server Logo" height="240" />


# Konnect *BETA Release

</div>

**AI-assisted PCB design for KiCAD 10.** Konnect is a native KiCAD plugin — a single
Rust binary — that lets Claude and other AI assistants design schematics and PCBs
through the [Model Context Protocol](https://modelcontextprotocol.io) (MCP).

**196 tools across 19 on-demand toolsets.** Schematic capture, PCB layout and
routing, ERC/DRC, design-review audits, JLCPCB part search, Freerouting, reference
circuits, and a full manufacturing export pipeline — with bundled skills and agents
that teach Claude KiCAD conventions out of the box.

> ### This is a fork
>
> [JYPochez/VNS-Kicad-Konnect](https://github.com/JYPochez/VNS-Kicad-Konnect) — a fork of
> [mixelpixx/Konnect](https://github.com/mixelpixx/Konnect) v0.2.2, carrying correctness
> fixes and a few tools found missing while using it on real hardware. Everything here is
> offered back upstream; the fork is not a divergence.
>
> **Fixes**
>
> - **Symbol libraries resolve through `sym-lib-table`.** `resolve_lib_symbol` scanned a
>   hardcoded list of install directories for a file named after the library nickname, so
>   every user library was invisible — and on any non-standard KiCad install (a macOS bundle
>   outside `/Applications`, say) *no symbol resolved at all*. Footprints got table-aware
>   resolution in v0.2.1; symbols never did. Now the table is read first, following
>   `(type "Table")` indirection and expanding `${KIPRJMOD}`, environment variables and the
>   user path variables set in Preferences → Configure Paths.
> - **Net queries are deterministic.** `net_at` returned the first label reached while
>   iterating a `HashMap`, so a net carrying two labels resolved differently between runs.
>   Results are now sorted and stable, and `nets_at` exposes the whole set so a conflicting
>   label becomes a reportable error instead of a coin-flip.
> - **Mid-wire query points connect.** A pin landing on a wire's interior, or any
>   `trace_from_point` coordinate, resolved to an isolated component and read as
>   unconnected.
> - **Union-find no longer overflows the stack.** Path compression recursed; a long parent
>   chain aborted the process, which no handler can catch. Now iterative, with union-by-size.
> - **A panicking tool no longer kills the server.** Handlers were awaited inline, so one
>   panic unwound out of `main` and took every other loaded tool with it.
> - **String escapes decode correctly.** Chained `replace` calls collapsed backslashes last,
>   so a serialized `C:\\new` came back as `C:\` followed by a real newline.
> - **Multi-unit symbols are handled as one component.** A multi-unit part is placed as one
>   instance *per unit*, all sharing the reference, and four tools took the **first** match:
>   `batch_connect_to_net` transformed every pin by unit 1's placement — silently landing two
>   nets on one coordinate and shorting them, with no error; `batch_edit_schematic_components`
>   wrote fields into unit 1 only; the delete tools left the other units behind as orphans;
>   `bulk_move` tore the part apart. Pin lookups now resolve against the unit that owns the
>   pin, and component-level edits apply to every unit.
> - **A schematic KiCad saved survives an edit.** Every tool that round-trips the document
>   through the typed model re-emitted the *whole* file: two-space indents where eeschema
>   writes tabs, closing parens inline, and blank lines inserted between top-level items.
>   Adding one net label to a 40,000-line schematic produced a 66,000-line diff, burying the
>   real change and discarding the formatting KiCad wrote. The writer now matches eeschema
>   exactly — including `pts` points wrapping after six and an embedded file's base64 kept
>   one chunk per line — so a 1.1 MB schematic round-trips byte-for-byte.
> - **`add_schematic_text` no longer writes an unopenable file.** It spliced the node in after
>   the symbol instances (KiCad 10 requires those last) and wrote literal newlines where the
>   format wants `\n`. Either one makes the whole schematic fail to load, while the tool
>   reports success.
>
> **Added tools**
>
> - **`sch_bus` toolset** — `add_bus`, `batch_add_bus`, `add_bus_entry`, and
>   `connect_pins_to_bus`. `SchematicBuilder` already round-tripped bus nodes, but nothing
>   could create one, so any repeated multi-signal link had to be one wire per signal or bare
>   labels. `connect_pins_to_bus` writes the stub, the entry *and* the member label per pin,
>   because bus membership in KiCad is by name — a stub without a label joins nothing.
> - `set_schematic_page` — sets the sheet size. Content outside the frame still exports and
>   still nets up, so an undersized page is a silent defect; the tool returns the size in mm
>   so the caller can check it against the layout.
> - `batch_add_no_connect` — `batch_delete_no_connect` existed with no batch add; marking one
>   MCU's unused pins is routinely 15–20 round trips.
> - `update_symbols_from_library` — eeschema's *Update Symbols from Library*. Refuses any
>   symbol whose pins moved, since wires and labels sit at the old coordinates.
> - `rename_project` — renames the project files *and* the internal references. Renaming
>   files alone orphans every reference designator, because each symbol instance stores
>   `(project "name")`.
> - `reload_server` — `exec`s into the rebuilt binary in place, keeping the PID and stdio
>   pipes so the MCP client's connection survives. Verifies the new binary first.
> - `set_schematic_field_geometry` — places a symbol's Reference/Value text: an offset from
>   the symbol origin plus a text angle. Nothing could move a field before —
>   `edit_schematic_component` changes only what a field *says*. The angle is stored relative
>   to its symbol, so a field left at 0 on a rotated symbol renders sideways while the file
>   still reads 0; omitting the angle now cancels the rotation.
>
> 533 tests; `cargo clippy --workspace -- -D warnings` clean.

> **Status: beta.** The core toolchain is tested and working, but this is a young
> release and it wants real-world mileage and review. Issues and PRs are welcome —
> see [CONTRIBUTING.md](CONTRIBUTING.md) and the
> [naming conventions](docs/NAMING_CONVENTIONS.md).

## Why Konnect exists

Konnect is the successor to [KiCAD-MCP-Server](https://github.com/mixelpixx/KiCAD-MCP-Server),
a Python/TypeScript project that proved AI-driven PCB design works — and, in the
process, showed exactly where that architecture runs out of road. Konnect was built
to fix those specific problems:

**The call path was too long.** In the original server, a single tool call travels
through TypeScript, schema validation, a spawned Python subprocess, JSON over
stdin/stdout, a command router, and finally SWIG-generated C++ proxy objects before
anything touches your board. That's four language and serialization boundaries, each
with its own failure modes — subprocess lifecycle management, stdout parsing that
filters out warnings KiCAD leaks into the stream, chunked-JSON reassembly. In
Konnect, a tool call is a function call. One process, one language, no plumbing.

**The dependency surface was enormous.** Running the original means carrying Node.js
and its npm tree, Python and its pip packages, wxPython, kicad-skip, and KiCAD's
SWIG bindings — two package ecosystems plus a binding layer, every one of them a
moving target that can break an install. Konnect is a single static binary, about
5 MB. There is nothing to install alongside it and nothing to version-match.

**SWIG is a dead end.** The original's PCB backend depends on KiCAD's SWIG Python
bindings, which KiCAD is deprecating in favor of its IPC API. SWIG also carried
real operational scars: a zone-fill call that can segfault the backend, proxy-object
comparison bugs, and a fallback path that can silently swap backends mid-session.
Konnect talks to KiCAD 10 through the official IPC API (protobuf over NNG) — the
interface KiCAD is investing in — with real-time board edits that integrate with
KiCAD's own undo/redo.

**Schematic edits should not corrupt files.** Konnect edits `.kicad_sch` files
through its own S-expression engine with atomic writes (write, fsync, rename), UUID
preservation, and round-trip tests — no third-party schematic library with known
gaps, no text-manipulation workarounds.

**Context economy is a feature.** Exposing ~180 tools to an LLM costs roughly 23K
tokens of context on every listing. Konnect's router loads a starter kit (~2K
tokens) and lets the model pull in toolsets on demand — plus built-in observability
(`get_recent_calls`, `server_stats`, JSONL call logs) so the model can diagnose its
own tool failures.

The result is smaller, faster to install, aligned with where KiCAD is going, and
built for production use rather than experimentation. The original project remains
open, maintained, and useful — see [the comparison below](#relationship-to-kicad-mcp-server).

## What it does

Instead of describing changes and applying them by hand, the AI works your project
directly:

- **Place and wire schematic components** — add resistors, ICs, connectors; wire them
  together by pin name
- **Lay out the PCB** — place, move, rotate, and route footprints in real time via
  KiCAD's IPC API, with full undo/redo integration
- **Run design checks** — ERC, DRC, connectivity validation, decoupling audits,
  power-rail review, BOM health checks
- **Export production files** — Gerbers, drill, BOM, pick-and-place, 3D models, PDF
- **Search JLCPCB parts** — find in-stock components in a local 2.5M-part catalog and
  suggest alternatives
- **Start from reference circuits** — USB-C, LDO, buck converter, STM32, I2C, LED
  templates with verified component values
- **Watch it happen** — a live schematic viewer auto-refreshes as the AI edits

The full tool catalog is documented in [tool-directory.md](tool-directory.md).

## How it works

| Layer | Mechanism |
|-------|-----------|
| Schematic editing | Direct `.kicad_sch` S-expression editing with atomic writes (no KiCAD required) |
| PCB editing | KiCAD 10 IPC API (NNG + protobuf) — real-time, undo-aware, requires KiCAD running |
| Exports & checks | `kicad-cli` subprocess (Gerber, PDF, ERC, DRC, …) |
| Transport | MCP JSON-RPC over stdio (default), or Streamable HTTP (`transport = "http"` / `"both"`) |

## Installation

### From the KiCAD Plugin Manager (recommended)

1. Download the package for your OS from [Releases](https://github.com/mixelpixx/Konnect/releases):
   `konnect-pcm-v<version>-windows.zip`, `-macos.zip`, or `-linux.zip`. Each
   bundles that platform's server binary — the macOS package is a universal
   build, so one download covers Apple Silicon and Intel. (The `konnect-pcm-*`
   assets are the KiCAD plugin packages; the other archives are standalone
   server binaries.)
2. Open KiCAD 10 → **Plugin and Content Manager**
3. Click **Install from File** and select the zip
4. Restart KiCAD

Verify: open the **PCB Editor** → **Tools → External Plugins** → you should see
**Konnect**.

### Build from source

```bash
# protoc is required (protobuf code generation), and cmake (the nng crate
# compiles the NNG C library with it).
# Windows: choco install protoc cmake
# macOS:   brew install protobuf cmake
# Linux:   apt install protobuf-compiler cmake
cargo build --release -p konnect
```

### macOS

The [Releases](https://github.com/mixelpixx/Konnect/releases) page ships
standalone server binaries for both Apple Silicon (`aarch64-apple-darwin`) and
Intel (`x86_64-apple-darwin`). They are not yet code-signed, so if you download
one through a browser, clear the quarantine flag before first launch:

```bash
tar xzf konnect-v*-aarch64-apple-darwin.tar.gz
xattr -d com.apple.quarantine ./konnect   # only needed for browser downloads
./konnect --help
```

Or build from source as above (verified on Apple Silicon; the same
`target/release/konnect` binary is the MCP server).

KiCad on macOS keeps its tools inside the app bundle and they are not on
`PATH`, so point Konnect at them in `~/Library/Application Support/konnect/config.toml`:

```toml
kicad_cli = "/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"
kicad_binary = "/Applications/KiCad/KiCad.app/Contents/MacOS/kicad"
# KiCad 10's IPC socket on macOS (enable it in KiCad:
# Preferences → Plugins → "Enable KiCad API")
ipc_address = "ipc:///tmp/kicad/api.sock"
```

Claude Desktop's config lives at
`~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "konnect": {
      "command": "/path/to/konnect"
    }
  }
}
```

For Claude Code, put the same snippet in a `.mcp.json` in your project root.

Starting with the next release, the PCM package for macOS
(`konnect-pcm-v<version>-macos.zip`) bundles a universal server binary; for
v0.1.3 and earlier, install via a release tarball or a source build. The schematic
viewer compiles and launches on macOS (Tauri 2 uses the system WKWebView —
WebView2 is only a Windows requirement) but hasn't had the same mileage as
the Windows build yet.

## Setup with Claude Desktop

After a PCM install, the server binary lives in your KiCAD documents folder:

```
C:\Users\<YOU>\Documents\KiCad\10.0\3rdparty\plugins\com_github_mixelpixx_konnect\bin\konnect.exe
```

Edit `%APPDATA%\Claude\claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "konnect": {
      "command": "C:\\Users\\<YOU>\\Documents\\KiCad\\10.0\\3rdparty\\plugins\\com_github_mixelpixx_konnect\\bin\\konnect.exe"
    }
  }
}
```

Restart Claude Desktop and the Konnect tools appear. For Claude Code, drop the same
snippet into a `.mcp.json` in your project root (see [examples/](examples/)).

## Schematic viewer

A standalone viewer that auto-refreshes as the schematic file changes:

```bash
schematic-viewer.exe path\to\your\root_schematic.kicad_sch
```

Point it at the root sheet of a hierarchical design and every sub-sheet is rendered
too, with a depth-indented sheet selector in the toolbar. Edits saved from KiCAD (or
made by the AI through the schematic tools) re-render only the sheets that changed
and refresh the view live — rendering runs against temp-folder snapshots, so the
viewer never blocks KiCAD from saving. Pan with click-drag, zoom with the wheel,
`0` to fit, `R` to refresh, drag-and-drop to open a different file. Also launchable
by the AI via the `open_schematic_viewer` tool.

Needs the WebView2 runtime (pre-installed on Windows 10/11) and a KiCAD install for
`kicad-cli` (auto-discovered, or pass `--kicad-cli <path>`). Built separately from
the main workspace — see [DEV.md](DEV.md) for build steps.

## Requirements

- KiCAD 10 (Windows is the most-tested platform; macOS works from the release
  binaries or a source build — see the [macOS section](#macos) above. Linux
  compiles and passes tests in CI but hasn't had per-platform QA yet; both are
  tracked on the [roadmap](ROADMAP.md))
- `kicad-cli` (ships with KiCAD — used for exports, ERC, DRC)
- For PCB tools: KiCAD running with the target board open (IPC API)

## License: free for the little guys

Konnect is licensed under the **[GNU AGPL-3.0](LICENSE)**.

If you're a hobbyist, student, freelancer, or open-source project: **use it freely,
no strings attached.** Design boards, ship them, sell them.

If you're a business: the AGPL requires that anything you build on or around Konnect —
including software provided over a network — be open-sourced under the same license.
If that doesn't work for you, **commercial licenses are available**: see
[COMMERCIAL.md](COMMERCIAL.md).

## Relationship to KiCAD-MCP-Server

The original [Python/TypeScript project](https://github.com/mixelpixx/KiCAD-MCP-Server)
remains fully open (MIT) and maintained. Konnect is where new development happens —
the architecture it proved, rebuilt for production:

| | KiCAD-MCP-Server | Konnect |
|---|---|---|
| Runtime | Node.js + Python + SWIG bindings | Single static binary (~5 MB) |
| Tool call path | TS → subprocess → Python → SWIG C++ | Direct function call |
| PCB backend | SWIG (deprecated by KiCAD) + experimental IPC | KiCAD 10 IPC API |
| Schematic backend | kicad-skip + custom loaders | Native S-expression engine, atomic writes |
| Context cost | Router pattern | Load/unload toolsets + observability |
| Skills / agents | — | 6 skills + 2 agents bundled |
| License | MIT | AGPL-3.0 + commercial |

## Troubleshooting

**Plugin doesn't appear in KiCAD** — install via the Plugin and Content Manager (not
manual copy), then restart KiCAD.

**PCB tools return "IPC connect failed"** — open KiCAD with your board file first;
PCB tools talk to the running PCB editor.

**"kicad-cli not found"** — common install paths are auto-detected; set the path
explicitly in the plugin settings dialog or your `konnect-settings.json` if yours
is elsewhere.

## Support

- Issues & feature requests: [GitHub Issues](https://github.com/mixelpixx/Konnect/issues)
- Roadmap: [ROADMAP.md](ROADMAP.md)
- Contributing: [CONTRIBUTING.md](CONTRIBUTING.md)
