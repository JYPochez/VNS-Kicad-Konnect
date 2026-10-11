//! `edit_schematic_component` — field edits and renames of placed symbols.
//!
//! Split out of `sch_components.rs`, which was past 100 KB. The tool is still
//! registered there; only its handler and helpers live here.

use crate::mcp::protocol::CallToolResult;
use crate::tools::{find_all_symbol_instance_blocks, get_path, opt_str, require_str, ToolContext};
use konnect_sexp::{
    commit_command, parse_sexp,
    writer::{apply_edits, read_consistent, SexpEdit},
    ItemId, SchematicCommand,
};
use serde_json::json;

pub(crate) async fn handle_edit_schematic_component(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let reference = match require_str(args, "reference") {
        Ok(r) => r.to_string(),
        Err(e) => return Ok(e),
    };
    let only_uuid = opt_str(args, "uuid");

    let content = read_consistent(&sch_path)?;
    let expected = content.clone();

    // Every placed block carrying this reference: a multi-unit part repeats it
    // once per unit, and a field edit belongs to the component, so it goes into
    // all of them. `uuid` narrows that to one placement — the only way to tell
    // apart two *different* symbols that ended up sharing a reference.
    let mut blocks: Vec<(usize, usize, String)> = Vec::new();
    for (start, end) in find_all_symbol_instance_blocks(&content, &reference) {
        let uuid = parse_sexp(&content[start..end])?
            .find_str("uuid")
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("component '{reference}' has a block without a UUID"))?;
        if only_uuid.is_none_or(|u| u == uuid) {
            blocks.push((start, end, uuid));
        }
    }
    if blocks.is_empty() {
        return Ok(CallToolResult::error(match only_uuid {
            Some(u) => format!("No symbol '{reference}' with uuid {u} in this schematic"),
            None => format!("symbol '{reference}' not found in this schematic"),
        }));
    }

    let new_reference = opt_str(args, "new_reference");
    if let Some(new_ref) = new_reference.filter(|r| *r != reference) {
        // Renaming onto a reference another component already uses is how a
        // duplicate is made, not fixed.
        if !find_all_symbol_instance_blocks(&content, new_ref).is_empty() {
            return Ok(CallToolResult::error(format!(
                "Reference '{new_ref}' is already used by another symbol"
            )));
        }
    }

    let fields: Vec<(&str, &str)> = [
        ("Reference", new_reference),
        ("Value", opt_str(args, "value")),
        ("Footprint", opt_str(args, "footprint")),
        ("Datasheet", opt_str(args, "datasheet")),
    ]
    .into_iter()
    .filter_map(|(field, v)| v.map(|v| (field, v)))
    .collect();
    if fields.is_empty() {
        return Ok(CallToolResult::error(
            "Nothing to change: give new_reference, value, footprint or datasheet",
        ));
    }

    let mut edits = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for &(start, end, _) in &blocks {
        let block = &content[start..end];
        for &(field, new_val) in &fields {
            match field_value_span(block, field) {
                Some((s, e)) => edits.push(SexpEdit::replace(
                    start + s,
                    start + e,
                    escape_sexp_string(new_val),
                )),
                None => errors.push(format!("'{reference}' has no '{field}' property")),
            }
        }
        // eeschema reads the reference from the (instances …) path entry, not
        // from the property: renaming only the field leaves the two disagreeing.
        if let Some(new_ref) = new_reference {
            let old = format!(r#"(reference "{reference}")"#);
            let mut from = 0;
            while let Some(rel) = block[from..].find(&old) {
                let s = start + from + rel;
                edits.push(SexpEdit::replace(
                    s,
                    s + old.len(),
                    format!(r#"(reference "{}")"#, escape_sexp_string(new_ref)),
                ));
                from += rel + old.len();
            }
        }
    }
    if !errors.is_empty() {
        errors.dedup();
        return Ok(CallToolResult::error(format!(
            "No fields were updated: {}",
            errors.join("; ")
        )));
    }

    let edited = apply_edits(content, edits);
    let ids = blocks
        .iter()
        .map(|(_, _, uuid)| ItemId::new(uuid.clone()))
        .collect::<Result<Vec<_>, _>>()?;
    let command = SchematicCommand::replace_items_from_document(
        &expected,
        &edited,
        ids,
        format!("Edit {reference}"),
    )?;
    commit_command(&sch_path, &command)?;

    Ok(CallToolResult::json(&json!({
        "reference": reference,
        "changes": fields.iter().map(|(f, v)| format!("{f} → {v}")).collect::<Vec<_>>(),
        "blocks": blocks.len(),
        "uuids": blocks.iter().map(|(_, _, u)| u.as_str()).collect::<Vec<_>>()
    })))
}

/// Byte span of the quoted value of `(property "<field>" "…")` within `block`,
/// excluding the quotes, honouring backslash escapes inside the value.
fn field_value_span(block: &str, field: &str) -> Option<(usize, usize)> {
    let open = format!(r#"(property "{field}" ""#);
    let start = block.find(&open)? + open.len();
    let bytes = block.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some((start, i)),
            _ => i += 1,
        }
    }
    None
}

/// Escape a value for a KiCad S-expression string.
fn escape_sexp_string(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::ToolRouter;
    use crate::tools::ServerConfig;
    use std::io::Write;

    /// One symbol block, tab-indented as eeschema saves it, with the
    /// (instances …) entry that carries the reference eeschema reads.
    fn block(lib_id: &str, uuid: &str, reference: &str, unit: u32) -> String {
        format!(
            "\t(symbol\n\t\t(lib_id \"{lib_id}\")\n\t\t(at 50 60 0)\n\t\t(unit {unit})\n\t\t(uuid \"{uuid}\")\n\t\t(property \"Reference\" \"{reference}\"\n\t\t\t(at 50 56 0)\n\t\t)\n\t\t(property \"Value\" \"GND\"\n\t\t\t(at 50 64 0)\n\t\t)\n\t\t(instances\n\t\t\t(project \"p\"\n\t\t\t\t(path \"/root\"\n\t\t\t\t\t(reference \"{reference}\")\n\t\t\t\t\t(unit {unit})\n\t\t\t\t)\n\t\t\t)\n\t\t)\n\t)\n"
        )
    }

    fn sheet(blocks: &[String]) -> String {
        format!(
            "(kicad_sch\n\t(version 20250610)\n\t(generator \"eeschema\")\n\t(uuid \"root\")\n{})\n",
            blocks.concat()
        )
    }

    async fn edit(sch: &str, args: serde_json::Value) -> (String, CallToolResult) {
        let mut f = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        f.write_all(sch.as_bytes()).unwrap();
        f.flush().unwrap();
        let mut args = args;
        args["schematic"] = json!(f.path().to_str().unwrap());
        let ctx = ToolContext::new(
            ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
            },
            std::sync::Arc::new(ToolRouter::new()),
        );
        let res = handle_edit_schematic_component(&args, &ctx).await.unwrap();
        (std::fs::read_to_string(f.path()).unwrap(), res)
    }

    /// The case that blocked annotation: two different power symbols both
    /// called #PWR025. `uuid` renames exactly one, field and instance alike.
    #[tokio::test]
    async fn uuid_renames_one_of_two_duplicates_in_field_and_instances() {
        let sch = sheet(&[
            block("power:+3V3", "pwr-a", "#PWR025", 1),
            block("power:GND", "pwr-b", "#PWR025", 1),
        ]);
        let (out, res) = edit(
            &sch,
            json!({ "reference": "#PWR025", "uuid": "pwr-b", "new_reference": "#PWR026" }),
        )
        .await;
        assert!(!res.is_error, "{res:?}");
        assert_eq!(
            out.matches("\"#PWR025\"").count(),
            2,
            "pwr-a keeps both copies:\n{out}"
        );
        assert_eq!(
            out.matches("\"#PWR026\"").count(),
            2,
            "pwr-b renamed in both:\n{out}"
        );
        let b = &out[out.find("pwr-b").unwrap()..];
        assert!(b.contains("(property \"Reference\" \"#PWR026\""));
        assert!(b.contains("(reference \"#PWR026\")"));
    }

    /// Without `uuid`, every unit of a multi-unit part is renamed — they are
    /// one component.
    #[tokio::test]
    async fn rename_without_uuid_covers_every_unit() {
        let sch = sheet(&[
            block("74xx:74HC14", "u-1", "U6", 1),
            block("74xx:74HC14", "u-2", "U6", 2),
        ]);
        let (out, res) = edit(&sch, json!({ "reference": "U6", "new_reference": "U7" })).await;
        assert!(!res.is_error, "{res:?}");
        assert!(!out.contains("\"U6\""), "no copy of U6 may remain:\n{out}");
        assert_eq!(out.matches("\"U7\"").count(), 4);
    }

    #[tokio::test]
    async fn rename_onto_a_reference_in_use_is_refused_and_writes_nothing() {
        let sch = sheet(&[
            block("Device:R", "r-1", "R1", 1),
            block("Device:R", "r-2", "R2", 1),
        ]);
        let (out, res) = edit(&sch, json!({ "reference": "R1", "new_reference": "R2" })).await;
        assert!(res.is_error);
        assert_eq!(out, sch);
    }

    #[tokio::test]
    async fn an_unknown_uuid_is_an_error() {
        let sch = sheet(&[block("Device:R", "r-1", "R1", 1)]);
        let (out, res) = edit(
            &sch,
            json!({ "reference": "R1", "uuid": "nope", "value": "1k" }),
        )
        .await;
        assert!(res.is_error);
        assert_eq!(out, sch);
    }

    #[tokio::test]
    async fn values_are_escaped() {
        let sch = sheet(&[block("Device:R", "r-1", "R1", 1)]);
        let (out, res) = edit(&sch, json!({ "reference": "R1", "value": "10\"k" })).await;
        assert!(!res.is_error, "{res:?}");
        assert!(out.contains(r#"(property "Value" "10\"k""#), "{out}");
    }
}
