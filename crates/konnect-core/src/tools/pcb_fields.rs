//! `move_footprint_text` — move a placed footprint's Reference or Value text.
//!
//! Kept out of `pcb_components.rs`, which is close to 100 KB; registered in the
//! same `pcb_components` toolset.

use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::pcb_components::with_ipc;
use crate::tools::{get_path, opt_f64, opt_str, require_str, ToolContext, ToolDef};
use konnect_ipc::client::{FieldPosition, FootprintField};
use serde_json::json;

pub fn tools() -> Vec<ToolDef> {
    vec![tool!(
        "move_footprint_text",
        "Move a placed footprint's Reference or Value text on the board via KiCAD IPC, \
         as pressing M on the text in pcbnew does — the footprint itself stays put. Give \
         either absolute board coordinates `x`/`y`, or an offset `dx`/`dy` from the \
         footprint's anchor (mm, board axes, +y down). `angle` sets the text angle on the \
         board (degrees); omit it to keep the current one. Typical use: clear a label \
         clipped by the board edge or sitting on a pad.",
        json!({
            "type": "object",
            "properties": {
                "board": { "type": "string" },
                "reference": { "type": "string", "description": "Footprint reference, e.g. 'J9'" },
                "field": { "type": "string", "enum": ["Reference", "Value"], "default": "Reference" },
                "x": { "type": "number", "description": "Absolute X (mm)" },
                "y": { "type": "number", "description": "Absolute Y (mm)" },
                "dx": { "type": "number", "description": "Offset from the footprint anchor (mm)" },
                "dy": { "type": "number", "description": "Offset from the footprint anchor (mm)" },
                "angle": { "type": "number", "description": "Text angle on the board, degrees (optional)" }
            },
            "required": ["board", "reference"]
        }),
        |args, ctx| async move { handle_move_footprint_text(args, ctx).await }
    )]
}

async fn handle_move_footprint_text(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let reference = match require_str(args, "reference") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let field = match opt_str(args, "field").unwrap_or("Reference") {
        "Reference" => FootprintField::Reference,
        "Value" => FootprintField::Value,
        other => {
            return Ok(CallToolResult::error(format!(
                "field must be 'Reference' or 'Value', got '{other}'"
            )))
        }
    };
    let pos = match (
        opt_f64(args, "x"),
        opt_f64(args, "y"),
        opt_f64(args, "dx"),
        opt_f64(args, "dy"),
    ) {
        (Some(x), Some(y), None, None) => FieldPosition::Absolute { x, y },
        (None, None, Some(dx), Some(dy)) => FieldPosition::Offset { dx, dy },
        _ => {
            return Ok(CallToolResult::error(
                "Give either x and y (absolute) or dx and dy (offset), not a mix",
            ))
        }
    };
    let angle = opt_f64(args, "angle");

    let board = get_path(args, "board")?;
    let addr = ctx.config.ipc_address.clone();
    let ref_ipc = reference.clone();
    let moved = with_ipc(addr, move |c| {
        c.ensure_board_is_active(&board)?;
        c.set_footprint_field_position(&ref_ipc, field, pos, angle)
    })
    .await?;
    match moved {
        Ok((x, y)) => Ok(CallToolResult::json(&json!({
            "reference": reference,
            "field": format!("{field:?}"),
            "x": x,
            "y": y,
            "angle": angle
        }))),
        Err(msg) => Ok(CallToolResult::error(format!(
            "KiCAD must be running with the board loaded (IPC error: {msg})"
        ))),
    }
}
