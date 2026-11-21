use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub file_extensions: Vec<String>,
    pub capabilities: Vec<PluginCapability>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum PluginCapability {
    Preview,
    Edit,
    Outline,
    Backlinks,
    Tasks,
    WikiLinks,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewPlugin {
    pub manifest: PluginManifest,
}

impl PreviewPlugin {
    pub fn markdown() -> Self {
        Self {
            manifest: PluginManifest {
                id: "kite.markdown".into(),
                name: "Markdown Live Preview".into(),
                version: "0.1.0".into(),
                file_extensions: vec!["md".into(), "markdown".into(), "mdx".into(), "mk".into()],
                capabilities: vec![
                    PluginCapability::Preview,
                    PluginCapability::Edit,
                    PluginCapability::Outline,
                    PluginCapability::Backlinks,
                    PluginCapability::Tasks,
                    PluginCapability::WikiLinks,
                ],
            },
        }
    }

    pub fn supports_path(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                self.manifest
                    .file_extensions
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(extension))
            })
    }
}

pub fn builtin_plugins() -> Vec<PreviewPlugin> {
    vec![PreviewPlugin::markdown()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_markdown_plugin_covers_markdown_and_mk() {
        let plugin = PreviewPlugin::markdown();
        assert!(plugin.supports_path(Path::new("README.md")));
        assert!(plugin.supports_path(Path::new("notes.MK")));
        assert!(!plugin.supports_path(Path::new("Makefile")));
        assert_eq!(builtin_plugins().len(), 1);
    }
}
