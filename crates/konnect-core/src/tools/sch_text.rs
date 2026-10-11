//! `edit_schematic_text` — change the wording of an existing schematic text.
//!
//! Registered in the `sch_batch` toolset next to `add_schematic_text`, which
//! could only create notes. The edit is a targeted replacement inside the
//! text's own string, so position, font and the rest of the file keep their
//! bytes.

use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::{get_path, require_str, ToolContext, ToolDef};
use konnect_sexp::writer::{
    apply_edits, find_block_starts, read_consistent, write_atomic_if_unchanged, SexpEdit,
};
use serde_json::json;

pub fn tools() -> Vec<ToolDef> {
    vec![tool!(
        "edit_schematic_text",
        "Replace a phrase inside an existing schematic text (a note placed with \
         add_schematic_text or in eeschema). `find` must occur in exactly one text item; \
         that occurrence becomes `replace`. Line breaks in the note are kept; write a line \
         break in find/replace as a real newline. Labels and symbol fields are not texts — \
         use the label and component tools for those.",
        json!({
            "type": "object",
            "properties": {
                "schematic": { "type": "string" },
                "find": { "type": "string", "description": "Phrase to find, as displayed" },
                "replace": { "type": "string", "description": "Replacement phrase" }
            },
            "required": ["schematic", "find", "replace"]
        }),
        |args, ctx| async move { handle_edit_schematic_text(args, ctx).await }
    )]
}

/// Escape plain text the way KiCad writes it inside a quoted string.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

/// Byte span of the first quoted string after `start` (quotes excluded).
fn quoted_span(content: &str, start: usize) -> Option<(usize, usize)> {
    let bytes = content.as_bytes();
    let open = start + content[start..].find('"')?;
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some((open + 1, i)),
            _ => i += 1,
        }
    }
    None
}

/// Apply the replacement to `content`. Pure, so it is testable without files.
pub(crate) fn edit_text(content: &str, find: &str, replace: &str) -> Result<String, String> {
    if find.is_empty() {
        return Err("`find` must not be empty".into());
    }
    let (find_raw, replace_raw) = (escape(find), escape(replace));
    let mut hits = Vec::new();
    for start in find_block_starts(content, "text") {
        let Some((s, e)) = quoted_span(content, start) else {
            continue;
        };
        let raw = &content[s..e];
        let n = raw.matches(&find_raw).count();
        if n > 0 {
            hits.push((s, e, n));
        }
    }
    match hits.as_slice() {
        [] => Err(format!("No schematic text contains '{find}'")),
        [(s, e, 1)] => {
            let new_raw = content[*s..*e].replacen(&find_raw, &replace_raw, 1);
            Ok(apply_edits(
                content.to_string(),
                vec![SexpEdit::replace(*s, *e, new_raw)],
            ))
        }
        [(_, _, n)] => Err(format!(
            "'{find}' occurs {n} times in that text — give a longer phrase"
        )),
        many => Err(format!(
            "'{find}' occurs in {} different texts — give a longer phrase",
            many.len()
        )),
    }
}

async fn handle_edit_schematic_text(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let find = match require_str(args, "find") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let replace = match require_str(args, "replace") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };
    let content = read_consistent(&sch_path)?;
    match edit_text(&content, &find, &replace) {
        Ok(new_content) => {
            write_atomic_if_unchanged(&sch_path, &content, &new_content)?;
            Ok(CallToolResult::json(
                &json!({ "replaced": find, "with": replace }),
            ))
        }
        Err(msg) => Ok(CallToolResult::error(msg)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCH: &str = "(kicad_sch\n\t(version 20250610)\n\t(text \"Vanne : jaune = N, marron = OUVERTURE, bleu = FERMETURE\\nligne 2 \\\"cite\\\"\"\n\t\t(exclude_from_sim no)\n\t\t(at 10 20 0)\n\t)\n\t(text \"Autre note : marron\"\n\t\t(at 30 40 0)\n\t)\n\t(label \"marron = OUVERTURE\"\n\t\t(at 1 1 0)\n\t)\n)\n";

    #[test]
    fn replaces_one_phrase_and_keeps_the_rest() {
        let out = edit_text(SCH, "marron = OUVERTURE", "rouge = OUVERTURE").unwrap();
        assert!(
            out.contains("jaune = N, rouge = OUVERTURE, bleu = FERMETURE\\nligne 2 \\\"cite\\\"")
        );
        // The label with the same words is not a text and is left alone.
        assert!(out.contains("(label \"marron = OUVERTURE\""));
        assert!(out.contains("(at 10 20 0)"));
    }

    #[test]
    fn newlines_and_quotes_in_find_match_the_escaped_file_form() {
        let out = edit_text(SCH, "FERMETURE\nligne 2 \"cite\"", "FERMETURE").unwrap();
        assert!(out.contains("bleu = FERMETURE\"\n"), "{out}");
    }

    #[test]
    fn ambiguous_or_missing_phrases_are_refused() {
        assert!(edit_text(SCH, "marron", "rouge").is_err(), "in two texts");
        assert!(edit_text(SCH, "violet", "rouge").is_err(), "nowhere");
        assert!(edit_text(SCH, "", "x").is_err());
    }
}
