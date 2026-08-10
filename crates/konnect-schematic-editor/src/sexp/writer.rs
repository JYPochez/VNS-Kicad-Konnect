use super::SexpNode;

/// Tags that get a blank line before them when emitted at depth 1.
const BLANK_BEFORE: &[&str] = &[
    "lib_symbols",
    "symbol",
    "wire",
    "bus",
    "bus_entry",
    "label",
    "global_label",
    "hierarchical_label",
    "junction",
    "no_connect",
    "net_tie",
    "polyline",
    "rectangle",
    "arc",
    "circle",
    "text",
    "text_box",
    "sheet",
    "sheet_instances",
    "symbol_instances",
];

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
                        let blank = child
                            .tag()
                            .map(|t| BLANK_BEFORE.contains(&t))
                            .unwrap_or(false);
                        if blank {
                            buf.push('\n');
                        }
                        buf.push('\n');
                        write_indent(buf, 1);
                        write_node(child, buf, 1);
                    }
                }
                buf.push('\n');
            } else if has_list_child {
                // Multi-line: scalars inline after tag, sub-lists on new lines.
                for (i, child) in children.iter().enumerate() {
                    if i == 0 {
                        write_node(child, buf, depth + 1);
                    } else if child.is_list() {
                        buf.push('\n');
                        write_indent(buf, depth + 1);
                        write_node(child, buf, depth + 1);
                    } else {
                        buf.push(' ');
                        write_node(child, buf, depth + 1);
                    }
                }
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

fn write_indent(buf: &mut String, depth: usize) {
    for _ in 0..depth {
        buf.push_str("  ");
    }
}
