use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::HostError;
use crate::host::PluginHostWarning;

pub const DISCOVERY_FILE_NAMES: [&str; 2] = ["pi-plugin-host.json", "plugin-host.json"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginLaunchDescriptor {
    pub id: String,
    pub name: String,
    pub executable: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl PluginLaunchDescriptor {
    pub fn validate(&self, path: &Path) -> Result<(), HostError> {
        if self.id.trim().is_empty() {
            return Err(HostError::InvalidDescriptor {
                path: path.to_path_buf(),
                message: "plugin id cannot be empty".to_string(),
            });
        }

        if self.name.trim().is_empty() {
            return Err(HostError::InvalidDescriptor {
                path: path.to_path_buf(),
                message: "plugin name cannot be empty".to_string(),
            });
        }

        if self.executable.as_os_str().is_empty() {
            return Err(HostError::InvalidDescriptor {
                path: path.to_path_buf(),
                message: "plugin executable cannot be empty".to_string(),
            });
        }

        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredPlugin {
    pub descriptor_path: PathBuf,
    pub descriptor: PluginLaunchDescriptor,
}

impl DiscoveredPlugin {
    pub fn base_dir(&self) -> &Path {
        self.descriptor_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, HostError> {
        let path = path.as_ref().to_path_buf();
        let descriptor = load_descriptor(&path)?;
        Ok(Self {
            descriptor_path: path,
            descriptor,
        })
    }
}

pub fn load_descriptor(path: impl AsRef<Path>) -> Result<PluginLaunchDescriptor, HostError> {
    let path = path.as_ref();
    let content = fs::read_to_string(path).map_err(|source| HostError::DescriptorRead {
        path: path.to_path_buf(),
        source,
    })?;
    let descriptor: PluginLaunchDescriptor =
        serde_json::from_str(&content).map_err(|source| HostError::DescriptorParse {
            path: path.to_path_buf(),
            source,
        })?;
    descriptor.validate(path)?;
    Ok(descriptor)
}

pub fn discover_plugins(roots: &[PathBuf]) -> Result<Vec<DiscoveredPlugin>, HostError> {
    let mut discovered = Vec::new();

    for root in roots {
        if root.is_file() {
            if is_descriptor_file(root) {
                discovered.push(DiscoveredPlugin::load(root)?);
            }
            continue;
        }

        if !root.exists() {
            continue;
        }

        for entry in WalkDir::new(root).follow_links(false) {
            let entry = entry.map_err(|source| HostError::Discovery {
                path: root.clone(),
                source,
            })?;
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.into_path();
            if is_descriptor_file(&path) {
                discovered.push(DiscoveredPlugin::load(path)?);
            }
        }
    }

    discovered.sort_by(|left, right| {
        left.descriptor
            .id
            .cmp(&right.descriptor.id)
            .then_with(|| left.descriptor_path.cmp(&right.descriptor_path))
    });
    Ok(discovered)
}

pub(crate) fn discover_plugins_with_warnings(
    roots: &[PathBuf],
) -> (Vec<DiscoveredPlugin>, Vec<PluginHostWarning>) {
    let mut discovered = Vec::new();
    let mut warnings = Vec::new();

    for root in roots {
        if root.is_file() {
            if is_descriptor_file(root) {
                match DiscoveredPlugin::load(root) {
                    Ok(plugin) => discovered.push(plugin),
                    Err(error) => warnings.push(PluginHostWarning {
                        path: root.clone(),
                        plugin_id: None,
                        plugin_name: None,
                        message: error.to_string(),
                    }),
                }
            }
            continue;
        }

        if !root.exists() {
            continue;
        }

        for entry in WalkDir::new(root).follow_links(false) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(source) => {
                    warnings.push(PluginHostWarning {
                        path: root.clone(),
                        plugin_id: None,
                        plugin_name: None,
                        message: source.to_string(),
                    });
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.into_path();
            if is_descriptor_file(&path) {
                match DiscoveredPlugin::load(&path) {
                    Ok(plugin) => discovered.push(plugin),
                    Err(error) => warnings.push(PluginHostWarning {
                        path: path.clone(),
                        plugin_id: None,
                        plugin_name: None,
                        message: error.to_string(),
                    }),
                }
            }
        }
    }

    discovered.sort_by(|left, right| {
        left.descriptor
            .id
            .cmp(&right.descriptor.id)
            .then_with(|| left.descriptor_path.cmp(&right.descriptor_path))
    });

    (discovered, warnings)
}

fn is_descriptor_file(path: &Path) -> bool {
    match path.file_name().and_then(|name| name.to_str()) {
        Some(name) => DISCOVERY_FILE_NAMES.contains(&name),
        None => false,
    }
}
