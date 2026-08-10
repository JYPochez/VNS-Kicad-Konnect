# KiCad bug report — ready to paste

Submit at: https://gitlab.com/kicad/code/kicad/-/issues/new (choose the **Bug report** template)

Everything below the line is the ticket body. Paste it as-is; only the two marked spots need your input.

---

## Title

```
IPC API: API_HANDLER_SCH leaves API_HANDLER_EDITOR::m_frame null, crashing eeschema in checkForBusy() (likely root cause of #24966)
```

## Description

`API_HANDLER_SCH` never initialises the base class's frame pointer and instead declares a **second, shadowing** `m_frame` member. Every editor command inherited from `API_HANDLER_EDITOR` calls `checkForBusy()`, which dereferences the **base** pointer — so any of those commands reaching the schematic handler dereferences null and takes down the whole KiCad process.

Because `KICAD_API_SERVER` dispatches a request to *every* registered handler in turn until one claims it, and `m_handlers` is a `std::set<API_HANDLER*>` (iteration order = pointer value, so effectively random per run), a request can reach the schematic handler even when it was meant for the board. **I believe this is the root cause of #24966** ("`Board.update_items()` intermittently crashes KiCad with null dereference in `_eeschema.dll`"), which would explain both the intermittency and why a *board* write crashes inside *eeschema*.

### Root cause

`API_HANDLER_EDITOR` owns the pointer that `checkForBusy()` uses:

```cpp
// include/api/api_handler_editor.h
API_HANDLER_EDITOR( EDA_BASE_FRAME* aFrame = nullptr );
EDA_BASE_FRAME* m_frame;
```

```cpp
// common/api/api_handler_editor.cpp
std::optional<ApiResponseStatus> API_HANDLER_EDITOR::checkForBusy()
{
    if( !m_frame->CanAcceptApiCommands() )   // <-- unguarded dereference
```

pcbnew initialises it correctly:

```cpp
// pcbnew/api/api_handler_pcb.cpp
API_HANDLER_PCB::API_HANDLER_PCB( PCB_EDIT_FRAME* aFrame ) :
        API_HANDLER_EDITOR( aFrame )          // OK — base m_frame is set
```

eeschema does not:

```cpp
// eeschema/api/api_handler_sch.cpp
API_HANDLER_SCH::API_HANDLER_SCH( SCH_EDIT_FRAME* aFrame ) :
        API_HANDLER_EDITOR(),                 // base m_frame = nullptr (default argument)
        m_frame( aFrame )                     // shadows the base member
```

```cpp
// eeschema/api/api_handler_sch.h
SCH_EDIT_FRAME* m_frame;                      // shadowing declaration
```

So `API_HANDLER_EDITOR::m_frame` is null for the lifetime of the schematic handler, while `API_HANDLER_SCH::m_frame` is valid but invisible to the base class.

### Affected commands

`API_HANDLER_EDITOR::registerHandlers()` registers all of these for every editor, and each calls `checkForBusy()` first:

- `HitTest`
- `CreateItems`
- `UpdateItems`
- `DeleteItems`
- `BeginCommit`

### Why it is intermittent

```cpp
// common/api/api_server.cpp — KICAD_API_SERVER::handleApiEvent()
for( API_HANDLER* handler : m_handlers )
{
    result = handler->Handle( request );
    if( result.has_value() )
        break;
    else if( result.error().status() != ApiStatusCode::AS_UNHANDLED )
        break;
}
```

```cpp
// include/api/api_server.h
std::set<API_HANDLER*> m_handlers;
```

`std::set` orders by pointer value, so with both editors open the schematic handler may be visited before the board handler on some runs and not others. When it is visited first for one of the five commands above, KiCad crashes.

## Reproduction steps

1. Open a project with the **Schematic Editor**.
2. Enable the API server (Preferences → Plugins → *Enable KiCad API*).
3. Run, with `kicad-python` installed:

```python
from kipy import KiCad
import kipy.proto.common.types.base_types_pb2 as bt
import kipy.proto.common.commands.editor_commands_pb2 as ec

k = KiCad(socket_path="ipc:///tmp/kicad/api.sock")   # adjust for your platform
sch = k.get_open_documents(bt.DOCTYPE_SCHEMATIC)[0]

m = ec.HitTest()
m.document.CopyFrom(sch)
k._client.send(m, None)      # eeschema crashes here
```

**Expected:** an error response — `AS_UNHANDLED`, or `AS_BUSY`, or a valid `HitTestResponse`.

**Actual:** the entire KiCad process crashes with a null-pointer dereference.

Read-only calls in the same session are unaffected: `ping`, `get_version`, `get_api_version`, `get_open_documents`, and the whole board read surface (`get_footprints`, `get_nets`, `get_tracks`, `get_zones`, `get_pads`, `get_stackup`) all work correctly.

## Stack trace

Captured on 10.0.2 (macOS, Apple Silicon). The defect is unchanged in the 10.0.5 source.

```
Thread 0 Crashed::  Dispatch queue: com.apple.main-thread
0   _eeschema.kiface   API_HANDLER_EDITOR::checkForBusy() + 24
1   _eeschema.kiface   API_HANDLER_EDITOR::handleHitTest(HANDLER_CONTEXT<kiapi::common::commands::HitTest> const&) + 76
2   _eeschema.kiface   void API_HANDLER::registerHandler<kiapi::common::commands::HitTest, kiapi::common::commands::HitTestResponse, API_HANDLER_EDITOR>(...)::'lambda'(kiapi::common::ApiRequest&)::operator()(kiapi::common::ApiRequest&) const + 348
3   libkicommon.10.0.2.dylib   API_HANDLER::Handle(kiapi::common::ApiRequest&) + 300
4   libkicommon.10.0.2.dylib   KICAD_API_SERVER::handleApiEvent(wxCommandEvent&) + 1164
5   libwx_osx_cocoau-3.2.0.4.1.dylib   wxEvtHandler::SearchDynamicEventTable(wxEvent&) + 368
6   libwx_osx_cocoau-3.2.0.4.1.dylib   wxEvtHandler::ProcessEventLocally(wxEvent&) + 92
7   libwx_osx_cocoau-3.2.0.4.1.dylib   wxEvtHandler::ProcessEvent(wxEvent&) + 112
8   libwx_osx_cocoau-3.2.0.4.1.dylib   wxEvtHandler::ProcessPendingEvents() + 324
9   libwx_osx_cocoau-3.2.0.4.1.dylib   wxAppConsoleBase::ProcessPendingEvents() + 176
10  libwx_osx_cocoau-3.2.0.4.1.dylib   wxCFEventLoop::OSXCommonModeObserverCallBack(__CFRunLoopObserver*, int, void*) + 88
11  CoreFoundation     __CFRUNLOOP_IS_CALLING_OUT_TO_AN_OBSERVER_CALLBACK_FUNCTION__ + 36
12  CoreFoundation     __CFRunLoopDoObservers + 536
13  CoreFoundation     __CFRunLoopRun + 784
14  CoreFoundation     CFRunLoopRunSpecific + 572
15  HIToolbox          RunCurrentEventLoopInMode + 324
16  HIToolbox          ReceiveNextEventCommon + 216
17  HIToolbox          _BlockUntilNextEventMatchingListInModeWithFilter + 76
18  AppKit             _DPSNextEvent + 684
19  AppKit             -[NSApplication(NSEventRouting) _nextEventMatchingEventMask:untilDate:inMode:dequeue:] + 688
20  AppKit             -[NSApplication run] + 480
21  libwx_osx_cocoau-3.2.0.4.1.dylib   wxGUIEventLoop::OSXDoRun() + 140
22  libwx_osx_cocoau-3.2.0.4.1.dylib   wxCFEventLoop::DoRun() + 40
23  libwx_osx_cocoau-3.2.0.4.1.dylib   wxEventLoopBase::Run() + 204
24  libwx_osx_cocoau-3.2.0.4.1.dylib   wxAppConsoleBase::MainLoop() + 212
25  libwx_osx_cocoau-3.2.0.4.1.dylib   wxApp::OnRun() + 36
26  kicad              APP_KICAD::OnRun() + 20
27  libwx_osx_cocoau-3.2.0.4.1.dylib   wxEntry(int&, wchar_t**) + 108
28  kicad              main + 52
29  dyld               start + 6076
```

## Suggested fix

Pass the frame to the base constructor and drop the shadowing member:

```diff
--- a/eeschema/api/api_handler_sch.cpp
+++ b/eeschema/api/api_handler_sch.cpp
 API_HANDLER_SCH::API_HANDLER_SCH( SCH_EDIT_FRAME* aFrame ) :
-        API_HANDLER_EDITOR(),
-        m_frame( aFrame )
+        API_HANDLER_EDITOR( aFrame )
 {
```

```diff
--- a/eeschema/api/api_handler_sch.h
+++ b/eeschema/api/api_handler_sch.h
-    SCH_EDIT_FRAME* m_frame;
```

Uses of `m_frame` inside `API_HANDLER_SCH` that need the derived type can go through a `static_cast<SCH_EDIT_FRAME*>( m_frame )` accessor, mirroring how `API_HANDLER_PCB` handles it.

Two hardening suggestions, independent of the above:

1. Make `checkForBusy()` null-safe, so a future handler that forgets the frame returns `AS_NOT_READY` instead of crashing the application:
   ```cpp
   if( !m_frame || !m_frame->CanAcceptApiCommands() )
   ```
2. Consider removing the `= nullptr` default argument on `API_HANDLER_EDITOR`'s constructor — it is what allowed this to compile silently.

## Version info

<!-- REPLACE: paste your Help → About → Copy Version Info from the KiCad GUI here -->

`kicad-cli version --format about`:

```
Application: kicad-cli arm64 on arm64

Version: 10.0.5, release build

Libraries:
	wxWidgets 3.2.8
	FreeType 2.14.3
	HarfBuzz 14.1.0
	FontConfig 2.17.1
	libcurl/8.7.1 (SecureTransport) LibreSSL/3.3.6 zlib/1.2.12 nghttp2/1.64.0

Platform: macOS Sequoia Version 15.7.7 (Build 24G720), 64 bit, Little endian, wxBase

Build Info:
	Date: Jul 21 2026 15:54:41
	wxWidgets: 3.2.8 (wchar_t,wx containers)
	Boost: 1.90.0
	OCC: 7.9.3
	Curl: 8.7.1
	ngspice: 45.2
	Compiler: Clang 16.0.0 with C++ ABI 1002
	KICAD_IPC_API=ON
	KICAD_USE_PCH=OFF
```

- Python: 3.11
- `kicad-python`: 0.7.1
- Affected versions: reproduced on 10.0.2; source inspection shows the defect unchanged in 10.0.3, 10.0.4, and 10.0.5.

## Related

- #24966 — `Board.update_items()` intermittently crashes KiCad with null dereference in `_eeschema.dll`. I believe this report identifies its root cause.
