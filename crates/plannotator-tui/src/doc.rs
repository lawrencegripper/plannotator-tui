//! Document model: the source text split into top-level Markdown blocks with byte ranges.
//!
//! The parser is `pulldown-cmark`; this module only walks its event stream at depth zero
//! and records where each block starts and ends. Everything downstream (rendering,
//! anchoring, hit-testing) is keyed by block index and byte range. Nothing here knows what
//! a heading or a list *is*.

use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockKind {
    Heading,
    Paragraph,
    List,
    CodeBlock,
    BlockQuote,
    Table,
    Rule,
    Html,
    /// YAML front matter; parsed for correct boundaries but never shown.
    Metadata,
    Other,
}

impl BlockKind {
    /// Code and tables keep their columns; everything else word-wraps.
    pub(crate) fn preserves_columns(self) -> bool {
        matches!(self, BlockKind::CodeBlock | BlockKind::Table)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Block {
    pub(crate) range: Range<usize>,
    pub(crate) kind: BlockKind,
}

#[derive(Debug)]
pub(crate) struct Document {
    pub(crate) source: String,
    pub(crate) blocks: Vec<Block>,
}

/// The option set `tui-markdown` enables, so block boundaries agree with its renderer.
fn parse_options() -> Options {
    Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_SUPERSCRIPT
        | Options::ENABLE_SUBSCRIPT
        | Options::ENABLE_MATH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_DEFINITION_LIST
        | Options::ENABLE_GFM
        | Options::ENABLE_TABLES
}

impl Document {
    pub(crate) fn parse(source: String) -> Self {
        let blocks = split_blocks(&source);
        Self { source, blocks }
    }

    /// Source text of block `index`. Empty for an out-of-range index.
    pub(crate) fn block_text(&self, index: usize) -> &str {
        self.blocks.get(index).map_or("", |b| &self.source[b.range.clone()])
    }

    /// The block whose range contains `offset`.
    pub(crate) fn block_containing(&self, offset: usize) -> Option<usize> {
        self.blocks.iter().position(|b| b.range.contains(&offset))
    }
}

fn kind_of(tag: &Tag<'_>) -> BlockKind {
    match tag {
        Tag::Heading { .. } => BlockKind::Heading,
        Tag::Paragraph => BlockKind::Paragraph,
        Tag::List(_) | Tag::Item => BlockKind::List,
        Tag::CodeBlock(_) => BlockKind::CodeBlock,
        Tag::BlockQuote(_) => BlockKind::BlockQuote,
        Tag::Table(_) => BlockKind::Table,
        Tag::HtmlBlock => BlockKind::Html,
        Tag::MetadataBlock(_) => BlockKind::Metadata,
        _ => BlockKind::Other,
    }
}

/// Walk the event stream and cut the source at depth-zero block boundaries.
fn split_blocks(source: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut depth = 0usize;
    // The block being read: where it starts, what it is, and the depth its End lands on.
    let mut open: Option<(usize, BlockKind, usize)> = None;

    for (event, range) in Parser::new_ext(source, parse_options()).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                // A top-level list is not a block; each of its items is, so a bullet can be
                // selected on its own. Whatever an item contains stays with that item.
                let opens = match depth {
                    0 => !matches!(tag, Tag::List(_)),
                    1 => matches!(tag, Tag::Item),
                    _ => false,
                };
                if opens {
                    open = Some((range.start, kind_of(&tag), depth));
                }
                depth += 1;
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if let Some((start, kind, _)) = open.take_if(|(_, _, closes_at)| *closes_at == depth) {
                    blocks.push(Block { range: start..range.end, kind });
                }
            }
            Event::Rule if depth == 0 => blocks.push(Block { range, kind: BlockKind::Rule }),
            // Any other depth-zero leaf (rare: stray html/text) becomes its own block.
            _ if depth == 0 => blocks.push(Block { range, kind: BlockKind::Other }),
            _ => {}
        }
    }

    // Trailing newlines are not part of a block's text: quotes stay stable across files
    // that differ only in final-newline conventions.
    for block in &mut blocks {
        let text = &source[block.range.clone()];
        let trimmed = text.trim_end_matches(['\n', '\r']);
        block.range.end = block.range.start + trimmed.len();
    }
    blocks.retain(|b| !b.range.is_empty() && b.kind != BlockKind::Metadata);
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_top_level_blocks_with_ranges() {
        let src = "# Title\n\nPara one\nstill one.\n\n- a\n- b\n\n```rs\nfn x() {}\n```\n\n---\n";
        let doc = Document::parse(src.to_owned());
        let kinds: Vec<_> = doc.blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            [
                BlockKind::Heading,
                BlockKind::Paragraph,
                BlockKind::List,
                BlockKind::List,
                BlockKind::CodeBlock,
                BlockKind::Rule
            ]
        );
        assert_eq!(doc.block_text(0), "# Title");
        assert_eq!(doc.block_text(1), "Para one\nstill one.");
        assert_eq!(doc.block_text(2), "- a");
        assert_eq!(doc.block_text(3), "- b");
        assert_eq!(doc.block_text(4), "```rs\nfn x() {}\n```");
    }

    #[test]
    fn front_matter_is_dropped_and_a_nested_list_stays_with_its_item() {
        let doc = Document::parse("---\ntitle: X\n---\n\n- a\n  - nested\n- b\n".to_owned());
        let texts: Vec<_> = (0..doc.blocks.len()).map(|i| doc.block_text(i)).collect();
        assert_eq!(texts, ["- a\n  - nested", "- b"]);
        assert!(doc.blocks.iter().all(|b| b.kind == BlockKind::List));
    }

    #[test]
    fn list_items_are_blocks_whether_tight_loose_or_numbered() {
        let doc = Document::parse("- a\n\n- b\n\n1. one\n2. two\n\npara\n".to_owned());
        let texts: Vec<_> = (0..doc.blocks.len()).map(|i| doc.block_text(i)).collect();
        assert_eq!(texts, ["- a", "- b", "1. one", "2. two", "para"]);
    }
}
