use super::SexpNode;

/// Tags whose list children share a line rather than each taking their own.
///
/// eeschema writes `(pts (xy …) (xy …))` with the points together, so a writer
/// that gave every `xy` its own line would reformat every wire in the file.
const INLINE_LIST_CHILDREN: &[&str] = &["pts"];

/// How many inline sub-lists eeschema puts on one line before wrapping.
const INLINE_CHILDREN_PER_LINE: usize = 6;

/// The tag of a list, for formatting decisions.
fn tag_of(children: &[SexpNode]) -> &str {
    match children.first() {
        Some(SexpNode::Atom(s)) => s.as_str(),
        _ => "",
    }
}

pub fn write(node: &SexpNode) -> String {
    let mut buf = String::with_capacity(16384);
    write_node(node, &mut buf, 0);
    buf.push('\n');
    buf
}

fn write_node(node: &SexpNode, buf: &mut String, depth: usize) {
    match node {
        SexpNode::Atom(s) => buf.push_str(s),
        SexpNode::Str(s) => {
            buf.push('"');
            for c in s.chars() {
                match c {
                    '"' => buf.push_str("\\\""),
                    '\\' => buf.push_str("\\\\"),
                    '\n' => buf.push_str("\\n"),
                    '\t' => buf.push_str("\\t"),
                    '\r' => buf.push_str("\\r"),
                    c => buf.push(c),
                }
            }
            buf.push('"');
        }
        SexpNode::List(children) => {
            if children.is_empty() {
                buf.push_str("()");
                return;
            }

            let has_list_child = children.iter().skip(1).any(|c| c.is_list());

            buf.push('(');

            if depth == 0 {
                // Root: tag on same line, each child on its own indented line.
                for (i, child) in children.iter().enumerate() {
                    if i == 0 {
                        write_node(child, buf, 1);
                    } else {
                        buf.push('\n');
                        write_indent(buf, 1);
                        write_node(child, buf, 1);
                    }
                }
                buf.push('\n');
            } else if tag_of(children) == "data" && children.len() > 2 {
                // An embedded file's base64 payload. eeschema splits it into
                // 76-character chunks, one per line; the parser hands them back
                // as one atom per chunk, so emitting one per line reproduces the
                // original exactly. Joining them onto a single line instead
                // rewrites thousands of lines whenever a datasheet is embedded.
                for (i, child) in children.iter().enumerate() {
                    match i {
                        0 => write_node(child, buf, depth + 1),
                        1 => {
                            buf.push(' ');
                            write_node(child, buf, depth + 1);
                        }
                        _ => {
                            buf.push('\n');
                            write_indent(buf, depth + 1);
                            write_node(child, buf, depth + 1);
                        }
                    }
                }
                buf.push('\n');
                write_indent(buf, depth);
            } else if has_list_child {
                // Multi-line: scalars inline after tag, sub-lists on new lines,
                // closing paren on its own line at the parent's indent.
                let inline_children = INLINE_LIST_CHILDREN.contains(&tag_of(children));
                for (i, child) in children.iter().enumerate() {
                    if i == 0 {
                        write_node(child, buf, depth + 1);
                    } else if child.is_list() {
                        // Tags such as `pts` keep their sub-lists on one line:
                        // eeschema writes `(xy …) (xy …)` together, wrapping
                        // after every INLINE_CHILDREN_PER_LINE of them.
                        if inline_children && (i - 1) % INLINE_CHILDREN_PER_LINE != 0 {
                            buf.push(' ');
                        } else {
                            buf.push('\n');
                            write_indent(buf, depth + 1);
                        }
                        write_node(child, buf, depth + 1);
                    } else {
                        buf.push(' ');
                        write_node(child, buf, depth + 1);
                    }
                }
                buf.push('\n');
                write_indent(buf, depth);
            } else {
                // All scalars: single line.
                for (i, child) in children.iter().enumerate() {
                    if i > 0 {
                        buf.push(' ');
                    }
                    write_node(child, buf, depth + 1);
                }
            }

            buf.push(')');
        }
    }
}

/// Indent with tabs, as eeschema does. Writing spaces here re-indents every
/// line of any file KiCad last saved, which buries the real change in a
/// whole-file diff.
fn write_indent(buf: &mut String, depth: usize) {
    for _ in 0..depth {
        buf.push('\t');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sexp::parser;

    /// Every construct whose formatting differs between writers, in the exact
    /// shape eeschema emits: tab indent, a closing paren on its own line for
    /// any multi-line list, `pts` points sharing a line and wrapping after six,
    /// an embedded-file payload split one base64 chunk per line, and all-scalar
    /// lists staying inline.
    const EESCHEMA_SHAPED: &str = "(kicad_sch\n\t(version 20260306)\n\t(paper \"A2\")\n\t(wire\n\t\t(pts\n\t\t\t(xy 45.72 325.12) (xy 60.96 325.12)\n\t\t)\n\t\t(uuid \"w-1\")\n\t)\n\t(polyline\n\t\t(pts\n\t\t\t(xy 0 0) (xy 1 1) (xy 2 2) (xy 3 3) (xy 4 4) (xy 5 5)\n\t\t\t(xy 6 6) (xy 7 7)\n\t\t)\n\t)\n\t(label \"DRO_CLK\"\n\t\t(at 99.06 335.28 270)\n\t\t(effects\n\t\t\t(font\n\t\t\t\t(size 1.27 1.27)\n\t\t\t)\n\t\t\t(justify right)\n\t\t)\n\t\t(uuid \"l-1\")\n\t)\n\t(embedded_files\n\t\t(file\n\t\t\t(name \"d.pdf\")\n\t\t\t(data |QUJD\n\t\t\t\tREVG\n\t\t\t\tR0hJ|\n\t\t\t)\n\t\t)\n\t)\n)\n";

    /// A schematic KiCad saved must survive a Konnect edit byte-for-byte apart
    /// from the edit itself. When the writer disagrees with eeschema about
    /// indentation or paren placement, every one of the file's lines shows up
    /// in the diff and the real change is impossible to review.
    #[test]
    fn round_trip_reproduces_eeschema_formatting_byte_for_byte() {
        let parsed = parser::parse(EESCHEMA_SHAPED).expect("fixture parses");
        let written = write(&parsed);

        assert_eq!(
            written, EESCHEMA_SHAPED,
            "writer output drifted from eeschema's formatting"
        );
    }

    #[test]
    fn indents_with_tabs_and_never_spaces() {
        let parsed = parser::parse(EESCHEMA_SHAPED).expect("fixture parses");
        let written = write(&parsed);

        assert!(
            !written.lines().any(|l| l.starts_with(' ')),
            "a space-indented line would re-indent the whole file on save"
        );
    }

    #[test]
    fn a_long_pts_wraps_after_six_points() {
        let parsed = parser::parse(EESCHEMA_SHAPED).expect("fixture parses");
        let written = write(&parsed);

        let counts: Vec<usize> = written
            .lines()
            .map(|l| l.matches("(xy ").count())
            .filter(|n| *n > 0)
            .collect();

        assert_eq!(
            counts,
            vec![2, 6, 2],
            "expected the wire's 2 points, then 6 + 2 for the wrapped polyline"
        );
    }

    #[test]
    fn an_embedded_payload_keeps_one_chunk_per_line() {
        let parsed = parser::parse(EESCHEMA_SHAPED).expect("fixture parses");
        let written = write(&parsed);

        assert!(
            written.contains("(data |QUJD\n"),
            "the first base64 chunk stays on the (data line"
        );
        assert!(
            !written.contains("QUJD REVG"),
            "chunks joined onto one line rewrite every line of an embedded file"
        );
    }

    #[test]
    fn an_empty_list_stays_inline() {
        let parsed = parser::parse("(kicad_sch\n\t(a)\n)\n").expect("parses");
        assert_eq!(write(&parsed), "(kicad_sch\n\t(a)\n)\n");
    }
}
