# Forum / Discord version — for relaying without a GitLab account

Shorter, narrative version of `kicad-bug-report-eeschema-api-null-frame.md`, written to be posted by
someone who cannot file on GitLab and is asking the community to relay it.

**Where to post**

| Route | URL | Account needed |
|---|---|---|
| KiCad Discord (best — devs reachable) | https://discord.gg/FANuKv8sZn | Discord account, free, no card |
| KiCad forum | https://forum.kicad.info/ | free, no card |
| IRC `#kicad` on Libera | https://web.libera.chat/#kicad | none |

Forum section: **Community**. Discord: the development/API channel.

---

## Subject

```
IPC API bug + patch: eeschema crashes in checkForBusy() — null m_frame in API_HANDLER_SCH (10.0.2–10.0.5) — can someone relay to GitLab?
```

## Body

I found a reproducible crash in the IPC API affecting KiCad 10.0.2 through 10.0.5, and I've traced it to
the root cause in the source with a two-line fix. I can't file on GitLab (account signup asks for
identity verification I'm not willing to complete), so I'm posting it here — **if someone with a GitLab
account is willing to relay this to the issue tracker, please do.** I believe it also explains an
already-open issue, #24966.

### What happens

Sending any of `HitTest`, `CreateItems`, `UpdateItems`, `DeleteItems` or `BeginCommit` over the IPC API
while the Schematic Editor is open crashes the entire KiCad process with a null-pointer dereference.

Minimal reproduction with `kicad-python` 0.7.1:

```python
from kipy import KiCad
import kipy.proto.common.types.base_types_pb2 as bt
import kipy.proto.common.commands.editor_commands_pb2 as ec

k = KiCad(socket_path="ipc:///tmp/kicad/api.sock")   # adjust per platform
sch = k.get_open_documents(bt.DOCTYPE_SCHEMATIC)[0]

m = ec.HitTest()
m.document.CopyFrom(sch)
k._client.send(m, None)      # eeschema crashes here
```

Read-only calls are all fine in the same session — `ping`, `get_version`, `get_open_documents`, and the
entire board read surface (`get_footprints`, `get_nets`, `get_tracks`, `get_zones`, `get_pads`,
`get_stackup`) work correctly.

Crash top frames (macOS, 10.0.2):

```
0  _eeschema.kiface  API_HANDLER_EDITOR::checkForBusy() + 24
1  _eeschema.kiface  API_HANDLER_EDITOR::handleHitTest(...) + 76
3  libkicommon       API_HANDLER::Handle(kiapi::common::ApiRequest&) + 300
4  libkicommon       KICAD_API_SERVER::handleApiEvent(wxCommandEvent&) + 1164
```

### Root cause

`API_HANDLER_EDITOR` owns the frame pointer that `checkForBusy()` dereferences:

```cpp
// include/api/api_handler_editor.h
API_HANDLER_EDITOR( EDA_BASE_FRAME* aFrame = nullptr );
EDA_BASE_FRAME* m_frame;

// common/api/api_handler_editor.cpp
if( !m_frame->CanAcceptApiCommands() )     // unguarded
```

pcbnew initialises it. eeschema does not — it passes nothing to the base constructor (which defaults to
`nullptr`) and declares a **second, shadowing** `m_frame`:

```cpp
// pcbnew — correct
API_HANDLER_PCB::API_HANDLER_PCB( PCB_EDIT_FRAME* aFrame ) :
        API_HANDLER_EDITOR( aFrame )

// eeschema — the bug
API_HANDLER_SCH::API_HANDLER_SCH( SCH_EDIT_FRAME* aFrame ) :
        API_HANDLER_EDITOR(),      // base m_frame stays null
        m_frame( aFrame )          // shadows the base member
```

So the base pointer is null for the handler's whole lifetime, and every inherited command that calls
`checkForBusy()` first will dereference it.

### Why it can hit you even when working on the PCB

`KICAD_API_SERVER::handleApiEvent()` offers each request to every registered handler until one claims it:

```cpp
for( API_HANDLER* handler : m_handlers )   // std::set<API_HANDLER*>
{
    result = handler->Handle( request );
    if( result.has_value() ) break;
    else if( result.error().status() != ApiStatusCode::AS_UNHANDLED ) break;
}
```

`m_handlers` is a `std::set`, so iteration order is pointer value — effectively random per run. With both
editors open, a board command can reach the *schematic* handler first and crash there.

**This looks like the explanation for open issue #24966** — *"`Board.update_items()` intermittently
crashes KiCad with null dereference in `_eeschema.dll`"*. It accounts for both the intermittency (set
ordering) and why a board write crashes inside eeschema.

### Suggested fix

```diff
--- a/eeschema/api/api_handler_sch.cpp
 API_HANDLER_SCH::API_HANDLER_SCH( SCH_EDIT_FRAME* aFrame ) :
-        API_HANDLER_EDITOR(),
-        m_frame( aFrame )
+        API_HANDLER_EDITOR( aFrame )

--- a/eeschema/api/api_handler_sch.h
-    SCH_EDIT_FRAME* m_frame;
```

Places inside `API_HANDLER_SCH` needing the derived type can use a `static_cast<SCH_EDIT_FRAME*>( m_frame )`
accessor, as `API_HANDLER_PCB` does.

Two hardening suggestions on top:

1. Make `checkForBusy()` null-safe so a future handler that forgets its frame returns `AS_NOT_READY`
   rather than killing the application: `if( !m_frame || !m_frame->CanAcceptApiCommands() )`
2. Drop the `= nullptr` default argument on the `API_HANDLER_EDITOR` constructor — it is what let this
   compile silently.

### Version

```
Version: 10.0.5, release build
Platform: macOS Sequoia 15.7.7 (24G720), 64 bit, Little endian
Build date: Jul 21 2026, Clang 16.0.0, KICAD_IPC_API=ON
Python 3.11, kicad-python 0.7.1
```

Crash captured on 10.0.2; source inspection shows the defect unchanged in 10.0.3, 10.0.4 and 10.0.5.
