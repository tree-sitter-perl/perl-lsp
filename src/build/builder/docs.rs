//! Sub documentation capture: preceding POD/comment extraction and the
//! tail-POD post-pass (`=head2`/`=item` sections trailing the code).

use super::*;

impl<'a> Builder<'a> {
    /// Extract POD or comment documentation immediately preceding a sub node.
    /// Walks prev_sibling chain (tree-sitter CST traversal stays in builder).
    pub(super) fn extract_preceding_doc(&self, sub_node: Node<'a>, sub_name: &str) -> Option<String> {
        let source_str = std::str::from_utf8(self.source).ok()?;
        let mut prev = self.syntax.prev_sibling(sub_node);
        let mut comment_lines: Vec<String> = Vec::new();

        while let Some(node) = prev {
            match node.kind() {
                "pod" => {
                    let text = &source_str[node.byte_range()];
                    // Try to extract just the section for this sub (=head2 or =item)
                    // extract_head2_section/extract_item_section return rendered markdown
                    if let Some(md) = crate::build::pod::extract_head2_section(sub_name, text) {
                        if !md.is_empty() {
                            return Some(md);
                        }
                    }
                    if let Some(md) = crate::build::pod::extract_item_section(sub_name, text) {
                        if !md.is_empty() {
                            return Some(md);
                        }
                    }
                    // Fallback: convert entire POD block (e.g. single-section pod)
                    let md = crate::build::pod::pod_to_markdown(text);
                    if !md.is_empty() {
                        return Some(md);
                    }
                    break;
                }
                "comment" => {
                    let text = source_str[node.byte_range()].trim();
                    let stripped = text.strip_prefix('#').unwrap_or(text).trim();
                    if !stripped.is_empty() {
                        comment_lines.push(stripped.to_string());
                    }
                }
                _ => break, // hit code, stop
            }
            prev = self.syntax.prev_sibling(node);
        }

        if !comment_lines.is_empty() {
            comment_lines.reverse(); // collected bottom-up
            return Some(comment_lines.join("\n"));
        }

        None
    }

    /// Post-pass: for subs with no preceding doc, scan collected pod_texts
    /// for a =head2 section matching the sub name (tail POD style).
    pub(super) fn resolve_tail_pod_docs(&mut self) {
        if self.pod_texts.is_empty() {
            return;
        }
        // Totals rather than a line per file — see `build.fold_iterations`.
        if crate::util::ghost_stats::enabled() {
            let nsubs = self.symbols.iter().filter(|s| matches!(s.kind, SymKind::Sub | SymKind::Method)).count();
            let podbytes: usize = self.pod_texts.iter().map(|p| p.len()).sum();
            crate::util::ghost_stats::count_by("build.tail_pod_subs", nsubs as u64);
            crate::util::ghost_stats::count_by("build.tail_pod_texts", self.pod_texts.len() as u64);
            crate::util::ghost_stats::count_by("build.tail_pod_bytes", podbytes as u64);
        }
        // One name → doc map across every POD block. Within a block `=head2`
        // wins over `=item`; across blocks the earlier `pod_text` wins
        // (`or_insert`), so a sub resolves to the first matching section in
        // source order.
        let mut sections: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for pod_text in &self.pod_texts {
            for (name, md) in crate::build::pod::extract_sub_doc_sections(pod_text) {
                sections.entry(name).or_insert(md);
            }
        }
        if sections.is_empty() {
            return;
        }
        for sym in &mut self.symbols {
            if !matches!(sym.kind, SymKind::Sub | SymKind::Method) {
                continue;
            }
            if let SymbolDetail::Sub { ref mut doc, .. } = sym.detail {
                if doc.is_some() {
                    continue; // already has preceding doc
                }
                if let Some(md) = sections.get(&sym.name) {
                    *doc = Some(md.clone());
                }
            }
        }
    }
}
