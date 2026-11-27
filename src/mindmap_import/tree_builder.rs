//! Small hierarchy builder shared by format adapters.

use crate::mindmap_import::model::{OutlineNode, StructureKind};

#[derive(Debug)]
pub(crate) struct OutlineTreeBuilder {
    root: OutlineNode,
    heading_stack: Vec<(u8, Vec<usize>)>,
    list_stack: Vec<(u8, Vec<usize>)>,
    current_path: Vec<usize>,
}

impl Default for OutlineTreeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl OutlineTreeBuilder {
    pub(crate) fn new() -> Self {
        Self {
            root: OutlineNode::structure("document", StructureKind::Document),
            heading_stack: Vec::new(),
            list_stack: Vec::new(),
            current_path: Vec::new(),
        }
    }

    pub(crate) fn append_heading(&mut self, level: u8, node: OutlineNode) -> Vec<usize> {
        let level = level.max(1);
        self.list_stack.clear();
        while self
            .heading_stack
            .last()
            .is_some_and(|(item_level, _)| *item_level >= level)
        {
            self.heading_stack.pop();
        }
        let parent_path = self
            .heading_stack
            .last()
            .map(|(_, path)| path.clone())
            .unwrap_or_default();
        let path = append_child(&mut self.root, &parent_path, node);
        self.heading_stack.push((level, path.clone()));
        self.current_path = path.clone();
        path
    }

    pub(crate) fn append_list_item(
        &mut self,
        level: u8,
        node: OutlineNode,
        max_depth: u8,
    ) -> Vec<usize> {
        let level = level.max(1);
        while self
            .list_stack
            .last()
            .is_some_and(|(item_level, _)| *item_level >= level)
        {
            self.list_stack.pop();
        }
        let mut parent_path = self
            .list_stack
            .last()
            .map(|(_, path)| path.clone())
            .or_else(|| self.heading_stack.last().map(|(_, path)| path.clone()))
            .unwrap_or_default();
        let maximum_parent_depth = usize::from(max_depth).saturating_sub(1);
        parent_path.truncate(maximum_parent_depth);
        let path = append_child(&mut self.root, &parent_path, node);
        self.list_stack.push((level, path.clone()));
        self.current_path = path.clone();
        path
    }

    pub(crate) fn ensure_body_section(&mut self, title: impl Into<String>) -> Vec<usize> {
        if self.current_path.is_empty() {
            self.append_heading(1, OutlineNode::structure(title, StructureKind::Section))
        } else {
            self.current_path.clone()
        }
    }

    pub(crate) fn current_path(&self) -> &[usize] {
        &self.current_path
    }

    pub(crate) fn close_list(&mut self) {
        if !self.list_stack.is_empty() {
            self.current_path = self
                .heading_stack
                .last()
                .map(|(_, path)| path.clone())
                .unwrap_or_default();
            self.list_stack.clear();
        }
    }

    pub(crate) fn current_node_mut(&mut self) -> Option<&mut OutlineNode> {
        node_at_path_mut(&mut self.root, &self.current_path)
    }

    pub(crate) fn node_mut(&mut self, path: &[usize]) -> Option<&mut OutlineNode> {
        node_at_path_mut(&mut self.root, path)
    }

    pub(crate) fn finish(self) -> Vec<OutlineNode> {
        self.root.children
    }
}

fn append_child(root: &mut OutlineNode, parent_path: &[usize], child: OutlineNode) -> Vec<usize> {
    let parent = if parent_path.is_empty() {
        root
    } else {
        node_at_path_mut(root, parent_path).expect("builder parent path must remain valid")
    };
    let mut path = parent_path.to_vec();
    path.push(parent.children.len());
    parent.children.push(child);
    path
}

fn node_at_path_mut<'a>(node: &'a mut OutlineNode, path: &[usize]) -> Option<&'a mut OutlineNode> {
    let Some((&index, rest)) = path.split_first() else {
        return Some(node);
    };
    node_at_path_mut(node.children.get_mut(index)?, rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_and_list_stacks_build_nested_paths() {
        let mut builder = OutlineTreeBuilder::new();
        builder.append_heading(1, OutlineNode::structure("H1", StructureKind::Section));
        builder.append_heading(2, OutlineNode::structure("H2", StructureKind::Section));
        builder.append_list_item(
            1,
            OutlineNode::structure(
                "L1",
                StructureKind::ListItem {
                    ordered: true,
                    level: 1,
                },
            ),
            8,
        );
        builder.append_list_item(
            2,
            OutlineNode::structure(
                "L2",
                StructureKind::ListItem {
                    ordered: true,
                    level: 2,
                },
            ),
            8,
        );
        let children = builder.finish();
        assert_eq!(children[0].title, "H1");
        assert_eq!(children[0].children[0].title, "H2");
        assert_eq!(children[0].children[0].children[0].title, "L1");
        assert_eq!(children[0].children[0].children[0].children[0].title, "L2");
    }
}
