//! `library` toolset, continued — footprint graphics and footprint copies.
//!
//! Split out of `library.rs`, which is already far past the size where one more
//! tool is comfortable. These tools are file-based like the rest of the
//! toolset: they edit `.kicad_mod` files and never touch a board.
//!
//! - `edit_footprint_graphics` lists, moves, deletes and adds the drawn items
//!   of a footprint — lines, rectangles, arcs, circles, polygons, texts and
//!   fields — on any layer (silkscreen, fab, courtyard, …).
//! - `copy_footprint` copies a footprint into another library under a new name,
//!   so a stock footprint can be varied without editing the stock library.
//!
//! Edits are targeted text replacements on the item's own sub-expressions, so
//! everything the tools do not touch keeps its exact bytes.

use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::library::{footprint_lib_dir, is_lib_id, resolve_footprint_path};
use crate::tools::{require_str, ToolContext, ToolDef};
use konnect_sexp::parser::{parse_sexp, SexpNode};
use konnect_sexp::writer::{
    apply_edits, find_balanced_block, find_block_with_leading_whitespace, find_direct_child_blocks,
    new_uuid, write_atomic, write_new_atomic, SexpEdit,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Heads of the footprint children this module treats as graphic items.
const GRAPHIC_HEADS: &[&str] = &[
    "fp_line",
    "fp_rect",
    "fp_arc",
    "fp_circle",
    "fp_poly",
    "fp_text",
    "property",
];

/// Point-valued sub-expressions an `edit` operation may rewrite.
const POINT_KEYS: &[&str] = &["start", "end", "mid", "center"];

// ─── Tool definitions ─────────────────────────────────────────────────────────

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool!(
            "edit_footprint_graphics",
            "List, edit, delete or add the drawn items of a .kicad_mod footprint — \
             fp_line / fp_rect / fp_arc / fp_circle / fp_poly / fp_text and fields \
             (Reference, Value…) — on any layer: silkscreen, fab, courtyard. Call it \
             with no `operations` to get the numbered item list; operations address \
             items by that `index`, always resolved against the file as it was before \
             this call, so deleting item 3 does not renumber item 5 within one call. \
             All operations are applied atomically. Coordinates are footprint-local mm \
             (origin = footprint anchor, usually pad 1). The board keeps its own copy \
             of each footprint: run 'Update Footprints from Library' in pcbnew \
             afterwards for placed parts to pick up the change.",
            json!({
                "type": "object",
                "properties": {
                    "footprint": { "type": "string", "description": "Path to a .kicad_mod file, or a Library:Footprint id" },
                    "project": { "type": "string", "description": "Optional .kicad_pro path, so a Library:Footprint id also resolves against the project fp-lib-table" },
                    "operations": {
                        "type": "array",
                        "description": "Operations to apply. Omit to only list the items.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "op": { "type": "string", "enum": ["edit", "delete", "add"] },
                                "index": { "type": "integer", "description": "edit/delete: item index from the listing" },
                                "kind": { "type": "string", "enum": ["line", "rect", "arc", "circle"], "description": "add: item type" },
                                "layer": { "type": "string", "description": "add (required) / edit: layer name, e.g. 'F.SilkS', 'F.Fab', 'F.CrtYd'" },
                                "start": { "type": "array", "items": { "type": "number" }, "description": "[x, y] mm — line/rect/arc start" },
                                "end": { "type": "array", "items": { "type": "number" }, "description": "[x, y] mm — line/rect/arc end; for a circle, a point on the circle" },
                                "mid": { "type": "array", "items": { "type": "number" }, "description": "[x, y] mm — arc midpoint" },
                                "center": { "type": "array", "items": { "type": "number" }, "description": "[x, y] mm — circle centre" },
                                "at": { "type": "array", "items": { "type": "number" }, "description": "edit a text or field: [x, y] or [x, y, angle]; the existing angle is kept when omitted" },
                                "width": { "type": "number", "description": "Stroke width mm (add default: 0.12 silkscreen, 0.05 courtyard, 0.1 otherwise)" },
                                "fill": { "type": "boolean", "description": "add rect/circle: filled (default false)" }
                            },
                            "required": ["op"]
                        }
                    }
                },
                "required": ["footprint"]
            }),
            |args, ctx| async move { handle_edit_footprint_graphics(args, ctx).await }
        ),
        tool!(
            "copy_footprint",
            "Copy a footprint into a footprint library under a new name — e.g. a stock \
             KiCad footprint into a personal library, to vary it with \
             edit_footprint_graphics / edit_footprint_pad without touching the stock \
             library. The copy keeps pads, graphics and 3D model; only its name (and \
             optionally its description) changes. Refuses to overwrite an existing \
             footprint unless `overwrite` is true.",
            json!({
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "Path to a .kicad_mod file, or a Library:Footprint id" },
                    "destination": { "type": "string", "description": "Target as Library:NewName (library from the fp-lib-table), or a .kicad_mod file path" },
                    "project": { "type": "string", "description": "Optional .kicad_pro path, so library ids also resolve against the project fp-lib-table" },
                    "description": { "type": "string", "description": "Optional new (descr …) for the copy" },
                    "overwrite": { "type": "boolean", "description": "Replace an existing footprint of that name (default false)" }
                },
                "required": ["source", "destination"]
            }),
            |args, ctx| async move { handle_copy_footprint(args, ctx).await }
        ),
    ]
}

// ─── Shared helpers ───────────────────────────────────────────────────────────

fn project_dir_of(args: &Value) -> Option<PathBuf> {
    args["project"]
        .as_str()
        .map(PathBuf::from)
        .and_then(|p| p.parent().map(Path::to_path_buf))
}

/// Format a millimetre value the way KiCad does: no trailing zeros, at most
/// six decimals, and never `-0`.
fn fmt_mm(v: f64) -> String {
    let s = format!("{:.6}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

fn point_arg(op: &Value, key: &str) -> anyhow::Result<Option<(f64, f64)>> {
    let Some(v) = op.get(key).filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let arr = v
        .as_array()
        .filter(|a| a.len() == 2 && a.iter().all(Value::is_number))
        .ok_or_else(|| anyhow::anyhow!("'{}' must be an [x, y] array of numbers", key))?;
    Ok(Some((arr[0].as_f64().unwrap(), arr[1].as_f64().unwrap())))
}

/// One graphic item of a footprint: its byte range in the file and parse tree.
struct GraphicItem {
    start: usize,
    end: usize,
    node: SexpNode,
}

impl GraphicItem {
    fn head(&self) -> &str {
        self.node.head().unwrap_or("")
    }
}

/// The footprint's graphic items, in file order.
fn graphic_items(content: &str) -> anyhow::Result<Vec<GraphicItem>> {
    let children = find_direct_child_blocks(content, "footprint");
    if children.is_empty() {
        anyhow::bail!("No (footprint …) block found — is this a .kicad_mod file?");
    }
    let mut items = Vec::new();
    for (start, end) in children {
        let node = parse_sexp(&content[start..end])
            .map_err(|e| anyhow::anyhow!("Unparsable footprint item: {:?}", e))?;
        if node.head().is_some_and(|h| GRAPHIC_HEADS.contains(&h)) {
            items.push(GraphicItem { start, end, node });
        }
    }
    Ok(items)
}

fn node_point(node: &SexpNode, key: &str) -> Option<[f64; 2]> {
    let n = node.find(key)?;
    Some([n.get_f64(1)?, n.get_f64(2)?])
}

/// JSON description of one item, as returned by the listing.
fn describe(index: usize, item: &GraphicItem) -> Value {
    let node = &item.node;
    let head = item.head();
    let mut d = json!({
        "index": index,
        "kind": head.trim_start_matches("fp_"),
        "layer": node.find_str("layer").unwrap_or(""),
    });
    for key in POINT_KEYS {
        if let Some(p) = node_point(node, key) {
            d[*key] = json!(p);
        }
    }
    if let Some(at) = node.find("at") {
        let vals: Vec<f64> = (1..4).filter_map(|i| at.get_f64(i)).collect();
        d["at"] = json!(vals);
    }
    if let Some(w) = node.find("stroke").and_then(|s| s.find_f64("width")) {
        d["width"] = json!(w);
    }
    if let Some(pts) = node.find("pts") {
        d["points"] = json!(pts.find_all("xy").len());
    }
    match head {
        "property" => {
            d["name"] = json!(node.get(1).and_then(SexpNode::as_str).unwrap_or(""));
            d["text"] = json!(node.get(2).and_then(SexpNode::as_str).unwrap_or(""));
        }
        "fp_text" => {
            d["name"] = json!(node.get(1).and_then(SexpNode::as_str).unwrap_or(""));
            d["text"] = json!(node.get(2).and_then(SexpNode::as_str).unwrap_or(""));
        }
        _ => {}
    }
    d
}

/// Absolute byte range of the direct child `(key …)` of the item, if present.
fn child_range(content: &str, item: &GraphicItem, key: &str) -> Option<(usize, usize)> {
    let block = &content[item.start..item.end];
    find_direct_child_blocks(block, item.head())
        .into_iter()
        .find(|&(s, _)| {
            let rest = &block[s + 1..];
            rest.starts_with(key)
                && rest[key.len()..].starts_with(|c: char| c.is_whitespace() || c == ')')
        })
        .map(|(s, e)| (item.start + s, item.start + e))
}

// ─── edit_footprint_graphics ──────────────────────────────────────────────────

/// Edits for one `edit` operation.
fn edit_item(
    content: &str,
    item: &GraphicItem,
    op: &Value,
    edits: &mut Vec<SexpEdit>,
) -> anyhow::Result<()> {
    let mut changed = false;
    for key in POINT_KEYS {
        if let Some((x, y)) = point_arg(op, key)? {
            let (s, e) = child_range(content, item, key)
                .ok_or_else(|| anyhow::anyhow!("A {} has no ({} …) to edit", item.head(), key))?;
            edits.push(SexpEdit::replace(
                s,
                e,
                format!("({} {} {})", key, fmt_mm(x), fmt_mm(y)),
            ));
            changed = true;
        }
    }
    if let Some(at) = op.get("at").filter(|v| !v.is_null()) {
        let vals: Vec<f64> = at
            .as_array()
            .filter(|a| (2..=3).contains(&a.len()))
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .filter(|v: &Vec<f64>| v.len() >= 2)
            .ok_or_else(|| anyhow::anyhow!("'at' must be [x, y] or [x, y, angle]"))?;
        let (s, e) = child_range(content, item, "at")
            .ok_or_else(|| anyhow::anyhow!("A {} has no (at …) to edit", item.head()))?;
        let angle = vals
            .get(2)
            .copied()
            .or_else(|| item.node.find("at").and_then(|a| a.get_f64(3)));
        let text = match angle {
            Some(a) => format!("(at {} {} {})", fmt_mm(vals[0]), fmt_mm(vals[1]), fmt_mm(a)),
            None => format!("(at {} {})", fmt_mm(vals[0]), fmt_mm(vals[1])),
        };
        edits.push(SexpEdit::replace(s, e, text));
        changed = true;
    }
    if let Some(w) = op.get("width").and_then(Value::as_f64) {
        let (ss, se) = child_range(content, item, "stroke")
            .ok_or_else(|| anyhow::anyhow!("A {} has no stroke to set a width on", item.head()))?;
        let stroke = &content[ss..se];
        let (ws, we) = find_direct_child_blocks(stroke, "stroke")
            .into_iter()
            .find(|&(s, _)| stroke[s..].starts_with("(width"))
            .ok_or_else(|| anyhow::anyhow!("Stroke without (width …)"))?;
        edits.push(SexpEdit::replace(
            ss + ws,
            ss + we,
            format!("(width {})", fmt_mm(w)),
        ));
        changed = true;
    }
    if let Some(layer) = op.get("layer").and_then(Value::as_str) {
        let (s, e) = child_range(content, item, "layer")
            .ok_or_else(|| anyhow::anyhow!("A {} has no (layer …) to edit", item.head()))?;
        edits.push(SexpEdit::replace(s, e, format!("(layer \"{}\")", layer)));
        changed = true;
    }
    if !changed {
        anyhow::bail!("edit operation changes nothing — give start/end/mid/center/at/width/layer");
    }
    Ok(())
}

fn default_width(layer: &str) -> f64 {
    if layer.ends_with("SilkS") || layer.ends_with("Silkscreen") {
        0.12
    } else if layer.ends_with("CrtYd") || layer.ends_with("Courtyard") {
        0.05
    } else {
        0.1
    }
}

/// The S-expression text of one new graphic item.
fn new_item_text(op: &Value) -> anyhow::Result<String> {
    let kind = op["kind"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("add needs 'kind' (line, rect, arc or circle)"))?;
    let layer = op["layer"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("add needs 'layer'"))?;
    let width = op["width"].as_f64().unwrap_or_else(|| default_width(layer));
    let fill = if op["fill"].as_bool().unwrap_or(false) {
        "yes"
    } else {
        "no"
    };
    let need = |key: &str| -> anyhow::Result<String> {
        let (x, y) =
            point_arg(op, key)?.ok_or_else(|| anyhow::anyhow!("add {} needs '{}'", kind, key))?;
        Ok(format!("({} {} {})", key, fmt_mm(x), fmt_mm(y)))
    };
    let geometry = match kind {
        "line" => format!("{} {}", need("start")?, need("end")?),
        "rect" => format!("{} {}", need("start")?, need("end")?),
        "arc" => format!("{} {} {}", need("start")?, need("mid")?, need("end")?),
        "circle" => format!("{} {}", need("center")?, need("end")?),
        other => anyhow::bail!("Unknown kind '{}' — use line, rect, arc or circle", other),
    };
    let fill_part = if matches!(kind, "rect" | "circle") {
        format!(" (fill {})", fill)
    } else {
        String::new()
    };
    Ok(format!(
        "(fp_{} {} (stroke (width {}) (type solid)){} (layer \"{}\") (uuid \"{}\"))",
        kind,
        geometry,
        fmt_mm(width),
        fill_part,
        layer,
        new_uuid()
    ))
}

/// Line prefix (newline + indentation) used by the item at `start`.
fn indentation_before(content: &str, start: usize) -> String {
    let line_start = content[..start].rfind('\n').map_or(0, |i| i + 1);
    let indent: String = content[line_start..start]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    format!("\n{}", indent)
}

/// Apply `operations` to a footprint's text. Pure, so it is testable without
/// touching the filesystem.
pub(crate) fn apply_graphic_ops(content: &str, operations: &[Value]) -> anyhow::Result<String> {
    let items = graphic_items(content)?;
    let item_at = |op: &Value| -> anyhow::Result<&GraphicItem> {
        let i = op["index"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("{} needs 'index'", op["op"]))? as usize;
        items.get(i).ok_or_else(|| {
            anyhow::anyhow!("No item {} — the footprint has {} items", i, items.len())
        })
    };

    // Added items go after the last drawn item, or before the first pad.
    let insert_at = match items.iter().rfind(|i| i.head() != "property") {
        Some(last) => last.end,
        None => items
            .last()
            .map(|i| i.end)
            .unwrap_or_else(|| content.rfind(')').expect("a footprint block ends with ')'")),
    };
    let indent = items
        .last()
        .map(|i| indentation_before(content, i.start))
        .unwrap_or_else(|| "\n  ".to_string());

    let mut edits = Vec::new();
    let mut deleted = std::collections::HashSet::new();
    let mut added = String::new();
    for (n, op) in operations.iter().enumerate() {
        let what = op["op"].as_str().unwrap_or("");
        let result = match what {
            "edit" => item_at(op).and_then(|item| edit_item(content, item, op, &mut edits)),
            "delete" => item_at(op).and_then(|item| {
                if !deleted.insert(item.start) {
                    anyhow::bail!("item {} is deleted twice", op["index"]);
                }
                let (s, e) = find_block_with_leading_whitespace(content, item.start)
                    .unwrap_or((item.start, item.end));
                edits.push(SexpEdit::delete(s, e));
                Ok(())
            }),
            "add" => new_item_text(op).map(|t| {
                added.push_str(&indent);
                added.push_str(&t);
            }),
            other => Err(anyhow::anyhow!(
                "Unknown op '{}' — use edit, delete or add",
                other
            )),
        };
        result.map_err(|e| anyhow::anyhow!("operation {}: {}", n, e))?;
    }
    // An edit inside a deleted item would collide with its deletion.
    for e in &edits {
        if e.replacement.is_empty() {
            continue;
        }
        if items
            .iter()
            .any(|i| deleted.contains(&i.start) && e.start >= i.start && e.end <= i.end)
        {
            anyhow::bail!("An item is both edited and deleted in the same call");
        }
    }
    if !added.is_empty() {
        edits.push(SexpEdit::insert(insert_at, added));
    }

    let out = apply_edits(content.to_string(), edits);
    // Refuse to write anything that no longer parses as one balanced block.
    let start = out.find('(').unwrap_or(0);
    match find_balanced_block(&out, start) {
        Some((_, end)) if out[end..].trim().is_empty() => {}
        _ => anyhow::bail!("Edit produced an unbalanced footprint — nothing was written"),
    }
    parse_sexp(&out).map_err(|e| anyhow::anyhow!("Edit produced unparsable output: {:?}", e))?;
    Ok(out)
}

async fn handle_edit_footprint_graphics(
    args: &Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let reference = require_str(args, "footprint").map_err(|e| anyhow::anyhow!("{:?}", e))?;
    let path = match resolve_footprint_path(reference, project_dir_of(args).as_deref()) {
        Ok(p) => p,
        Err(msg) => return Ok(CallToolResult::error(msg)),
    };
    let mut content = tokio::fs::read_to_string(&path).await?;

    let operations = args["operations"].as_array().cloned().unwrap_or_default();
    if !operations.is_empty() {
        content = match apply_graphic_ops(&content, &operations) {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(e.to_string())),
        };
        write_atomic(&path, &content)?;
    }

    let listing: Vec<Value> = graphic_items(&content)?
        .iter()
        .enumerate()
        .map(|(i, item)| describe(i, item))
        .collect();
    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "success": true,
            "footprint": path.to_str().unwrap_or(""),
            "operations_applied": operations.len(),
            "items": listing
        }))
        .unwrap(),
    ))
}

// ─── copy_footprint ───────────────────────────────────────────────────────────

/// Where a copy goes: a Library:Name id resolved to its directory, or a path.
fn destination_path(dest: &str, project_dir: Option<&Path>) -> Result<PathBuf, String> {
    if is_lib_id(dest) {
        let (nick, name) = dest.split_once(':').expect("checked by is_lib_id");
        let dir = footprint_lib_dir(nick, project_dir)?;
        return Ok(dir.join(format!("{name}.kicad_mod")));
    }
    let path = PathBuf::from(dest);
    if path.extension().and_then(|e| e.to_str()) != Some("kicad_mod") {
        return Err(format!(
            "Destination '{}' is neither a Library:Name id nor a .kicad_mod path",
            dest
        ));
    }
    Ok(path)
}

/// Rename the footprint (and optionally replace its description) in `content`.
pub(crate) fn renamed_footprint(
    content: &str,
    new_name: &str,
    description: Option<&str>,
) -> anyhow::Result<String> {
    let start = content
        .find("(footprint")
        .ok_or_else(|| anyhow::anyhow!("No (footprint …) block in the source"))?;
    let root = parse_sexp(&content[start..])
        .map_err(|e| anyhow::anyhow!("Unparsable source footprint: {:?}", e))?;
    let old_name = root
        .get(1)
        .and_then(SexpNode::as_str)
        .ok_or_else(|| anyhow::anyhow!("Footprint has no name"))?;
    let old_head = format!("(footprint \"{}\"", old_name);
    if !content[start..].starts_with(&old_head) {
        anyhow::bail!("Unexpected footprint header — expected {}", old_head);
    }
    let escaped = new_name.replace('\\', "\\\\").replace('"', "\\\"");
    let mut edits = vec![SexpEdit::replace(
        start,
        start + old_head.len(),
        format!("(footprint \"{}\"", escaped),
    )];
    if let Some(descr) = description {
        let descr = descr.replace('\\', "\\\\").replace('"', "\\\"");
        let block = &content[start..];
        let (ds, de) = find_direct_child_blocks(block, "footprint")
            .into_iter()
            .find(|&(s, _)| block[s..].starts_with("(descr"))
            .ok_or_else(|| anyhow::anyhow!("Source has no (descr …) to replace"))?;
        edits.push(SexpEdit::replace(
            start + ds,
            start + de,
            format!("(descr \"{}\")", descr),
        ));
    }
    Ok(apply_edits(content.to_string(), edits))
}

async fn handle_copy_footprint(args: &Value, _ctx: &ToolContext) -> anyhow::Result<CallToolResult> {
    let source = require_str(args, "source").map_err(|e| anyhow::anyhow!("{:?}", e))?;
    let dest = require_str(args, "destination").map_err(|e| anyhow::anyhow!("{:?}", e))?;
    let project_dir = project_dir_of(args);

    let src_path = match resolve_footprint_path(source, project_dir.as_deref()) {
        Ok(p) => p,
        Err(msg) => return Ok(CallToolResult::error(msg)),
    };
    let dst_path = match destination_path(dest, project_dir.as_deref()) {
        Ok(p) => p,
        Err(msg) => return Ok(CallToolResult::error(msg)),
    };
    let new_name = dst_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();

    let content = tokio::fs::read_to_string(&src_path).await?;
    let copy = match renamed_footprint(&content, &new_name, args["description"].as_str()) {
        Ok(c) => c,
        Err(e) => return Ok(CallToolResult::error(e.to_string())),
    };

    let overwrite = args["overwrite"].as_bool().unwrap_or(false);
    if overwrite {
        write_atomic(&dst_path, &copy)?;
    } else if dst_path.exists() {
        return Ok(CallToolResult::error(format!(
            "{} already exists — pass overwrite: true to replace it",
            dst_path.display()
        )));
    } else {
        write_new_atomic(&dst_path, &copy)?;
    }

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "success": true,
            "source": src_path.to_str().unwrap_or(""),
            "footprint": new_name,
            "output": dst_path.to_str().unwrap_or("")
        }))
        .unwrap(),
    ))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A footprint in the compact one-item-per-line form the JY library uses.
    const COMPACT: &str = "(footprint \"RV\"\n  (layer \"F.Cu\")\n  (descr \"old\")\n  \
(pad \"1\" thru_hole circle (at -2.5 0) (size 2.6 2.6) (layers \"*.Cu\" \"*.Mask\") (drill 1.3))\n  \
(fp_rect (start -3.65 -1.15) (end 3.65 1.15) (stroke (width 0.12) (type solid)) (fill none) (layer \"F.SilkS\"))\n  \
(fp_circle (center -4.05 0) (end -3.9 0) (stroke (width 0.1) (type solid)) (fill solid) (layer \"F.SilkS\"))\n  \
(fp_text reference \"REF**\" (at 0 -2.5 0) (layer \"F.SilkS\") (effects (font (size 1 1) (thickness 0.15))))\n)\n";

    /// The same kind of content in KiCad's own multi-line, tab-indented form.
    const KICAD: &str = "(footprint \"DIP\"\n\t(layer \"F.Cu\")\n\t(descr \"dip\")\n\
\t(property \"Reference\" \"REF**\"\n\t\t(at 3.81 -2.33 0)\n\t\t(layer \"F.SilkS\")\n\t)\n\
\t(fp_line\n\t\t(start 1.16 -1.33)\n\t\t(end 2.81 -1.33)\n\t\t(stroke\n\t\t\t(width 0.12)\n\t\t\t(type solid)\n\t\t)\n\t\t(layer \"F.SilkS\")\n\t)\n\
\t(fp_arc\n\t\t(start 4.81 -1.33)\n\t\t(mid 3.81 -0.33)\n\t\t(end 2.81 -1.33)\n\t\t(stroke\n\t\t\t(width 0.12)\n\t\t\t(type solid)\n\t\t)\n\t\t(layer \"F.SilkS\")\n\t)\n\
\t(pad \"1\" thru_hole roundrect\n\t\t(at 0 0)\n\t\t(size 1.6 1.6)\n\t)\n)\n";

    fn listing(content: &str) -> Vec<Value> {
        graphic_items(content)
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, it)| describe(i, it))
            .collect()
    }

    #[test]
    fn lists_items_in_file_order_without_pads() {
        let l = listing(COMPACT);
        assert_eq!(l.len(), 3);
        assert_eq!(l[0]["kind"], "rect");
        assert_eq!(l[0]["start"], json!([-3.65, -1.15]));
        assert_eq!(l[1]["kind"], "circle");
        assert_eq!(l[2]["kind"], "text");
        assert_eq!(l[2]["at"], json!([0.0, -2.5, 0.0]));

        let k = listing(KICAD);
        assert_eq!(k.len(), 3);
        assert_eq!(k[0]["kind"], "property");
        assert_eq!(k[0]["name"], "Reference");
        assert_eq!(k[2]["mid"], json!([3.81, -0.33]));
    }

    #[test]
    fn edits_points_width_and_keeps_text_angle() {
        let ops = vec![
            json!({"op": "edit", "index": 0, "start": [-6.11, -2.36], "end": [6.11, 2.36]}),
            json!({"op": "edit", "index": 1, "center": [-6.6, 0], "end": [-6.45, 0], "width": 0.2}),
            json!({"op": "edit", "index": 2, "at": [0, -3.4]}),
        ];
        let out = apply_graphic_ops(COMPACT, &ops).unwrap();
        let l = listing(&out);
        assert_eq!(l[0]["start"], json!([-6.11, -2.36]));
        assert_eq!(l[0]["end"], json!([6.11, 2.36]));
        assert_eq!(l[1]["center"], json!([-6.6, 0.0]));
        assert_eq!(l[1]["width"], json!(0.2));
        assert_eq!(l[2]["at"], json!([0.0, -3.4, 0.0]));
        // Untouched sub-expressions keep their bytes.
        assert!(out.contains("(fill none)"));
        assert!(out.contains("(pad \"1\" thru_hole circle (at -2.5 0)"));
    }

    #[test]
    fn edits_multiline_kicad_items() {
        let ops = vec![
            json!({"op": "edit", "index": 1, "end": [2.2, -1.33]}),
            json!({"op": "edit", "index": 0, "at": [3.81, -3.6]}),
        ];
        let out = apply_graphic_ops(KICAD, &ops).unwrap();
        let l = listing(&out);
        assert_eq!(l[1]["end"], json!([2.2, -1.33]));
        assert_eq!(l[0]["at"], json!([3.81, -3.6, 0.0]));
        // The pad's (at 0 0) is not mistaken for the property's.
        assert!(out.contains("\t(pad \"1\" thru_hole roundrect\n\t\t(at 0 0)"));
    }

    #[test]
    fn deletes_and_adds_with_indices_from_the_original_file() {
        let ops = vec![
            json!({"op": "delete", "index": 2}),
            json!({"op": "add", "kind": "line", "layer": "F.SilkS", "start": [5.42, -1.33], "end": [6.46, -1.33]}),
            json!({"op": "add", "kind": "circle", "layer": "F.SilkS", "center": [-1.4, 0], "end": [-1.25, 0], "fill": true}),
            json!({"op": "edit", "index": 1, "end": [2.2, -1.33]}),
        ];
        let out = apply_graphic_ops(KICAD, &ops).unwrap();
        let l = listing(&out);
        let kinds: Vec<&str> = l.iter().map(|d| d["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["property", "line", "line", "circle"]);
        assert_eq!(l[1]["end"], json!([2.2, -1.33]));
        assert_eq!(l[2]["start"], json!([5.42, -1.33]));
        assert_eq!(l[2]["width"], json!(0.12));
        assert!(out.contains("(fill yes)"));
        assert!(!out.contains("fp_arc"));
        // New items take the file's own indentation.
        assert!(out.contains("\n\t(fp_line (start 5.42 -1.33)"));
    }

    #[test]
    fn rejects_bad_operations_without_partial_output() {
        let bad_index = vec![json!({"op": "delete", "index": 9})];
        assert!(apply_graphic_ops(COMPACT, &bad_index).is_err());
        let no_mid = vec![json!({"op": "edit", "index": 0, "mid": [0, 0]})];
        assert!(apply_graphic_ops(COMPACT, &no_mid).is_err());
        let both = vec![
            json!({"op": "delete", "index": 0}),
            json!({"op": "edit", "index": 0, "start": [0, 0]}),
        ];
        assert!(apply_graphic_ops(COMPACT, &both).is_err());
        let noop = vec![json!({"op": "edit", "index": 0})];
        assert!(apply_graphic_ops(COMPACT, &noop).is_err());
    }

    #[test]
    fn copy_renames_and_redescribes() {
        let out = renamed_footprint(KICAD, "DIP_Fente", Some("avec \"fente\"")).unwrap();
        assert!(out.starts_with("(footprint \"DIP_Fente\"\n"));
        assert!(out.contains("(descr \"avec \\\"fente\\\"\")"));
        assert!(out.contains("(pad \"1\" thru_hole roundrect"));
        let same = renamed_footprint(KICAD, "X", None).unwrap();
        assert!(same.contains("(descr \"dip\")"));
    }

    #[test]
    fn formats_like_kicad() {
        assert_eq!(fmt_mm(2.2), "2.2");
        assert_eq!(fmt_mm(-0.0000001), "0");
        assert_eq!(fmt_mm(6.0), "6");
        assert_eq!(fmt_mm(-1.33), "-1.33");
    }
}
