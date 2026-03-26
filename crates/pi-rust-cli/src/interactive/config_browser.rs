use super::*;
use pi_rust_packages::PackageInstallScope;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum ConfigResourceKind {
    Skill,
    Prompt,
    Theme,
}

impl ConfigResourceKind {
    fn label(self) -> &'static str {
        match self {
            Self::Skill => "Skills",
            Self::Prompt => "Prompts",
            Self::Theme => "Themes",
        }
    }

    fn settings_key(self) -> &'static str {
        match self {
            Self::Skill => "skills",
            Self::Prompt => "prompts",
            Self::Theme => "themes",
        }
    }

    fn resource_kind(self) -> pi_rust_resources::ResourceKind {
        match self {
            Self::Skill => pi_rust_resources::ResourceKind::Skills,
            Self::Prompt => pi_rust_resources::ResourceKind::Prompts,
            Self::Theme => pi_rust_resources::ResourceKind::Themes,
        }
    }

    fn package_resource_kind(self) -> pi_rust_packages::PackageResourceKind {
        match self {
            Self::Skill => pi_rust_packages::PackageResourceKind::Skills,
            Self::Prompt => pi_rust_packages::PackageResourceKind::Prompts,
            Self::Theme => pi_rust_packages::PackageResourceKind::Themes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ConfigBrowserOwner {
    TopLevel {
        scope: SettingsScope,
        base_dir: PathBuf,
    },
    Package {
        identity: String,
        scope: PackageInstallScope,
        source: String,
        install_path: PathBuf,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ConfigBrowserItem {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) path: PathBuf,
    pub(super) enabled: bool,
    pub(super) kind: ConfigResourceKind,
    pub(super) owner: ConfigBrowserOwner,
    pub(super) searchable: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfigBrowserSection {
    title: &'static str,
    items: Vec<ConfigBrowserItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfigBrowserGroup {
    title: String,
    sections: Vec<ConfigBrowserSection>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ConfigBrowserData {
    groups: Vec<ConfigBrowserGroup>,
    has_extension_config: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ConfigBrowserVisibleRow {
    title: String,
    pub(super) item: ConfigBrowserItem,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ConfigBrowserDisplayRow {
    Group(String),
    Section(&'static str),
    Item(ConfigBrowserItem),
    Blank,
}

pub(super) fn run_config_tui() -> Result<(), String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut package_manager = PackageManager::create(&cwd, None);
    let mut terminal = ProcessTerminal::new();
    terminal.start().map_err(|error| error.to_string())?;
    terminal
        .set_title("Resource Configuration")
        .map_err(|error| error.to_string())?;
    terminal.hide_cursor().map_err(|error| error.to_string())?;

    let mut renderer = LineDiffRenderer::new(RenderAnchor { col: 0, row: 0 });
    let mut search = Input::with_prompt("> ");
    search.set_focused(true);
    let mut status = None;
    let mut selected_id: Option<String> = None;

    loop {
        let (width, height) = terminal.size().map_err(|error| error.to_string())?;
        let data = build_config_browser_data(&package_manager);
        let visible_rows = build_config_browser_visible_rows(&data, search.get_value());
        selected_id = sync_config_browser_selection(&visible_rows, selected_id.as_deref());
        let output = render_config_browser(
            &search,
            &data,
            &visible_rows,
            selected_id.as_deref(),
            status.as_deref(),
            width,
            height,
        );
        renderer
            .render(&mut terminal, &output, width)
            .map_err(|error| error.to_string())?;

        let events = terminal.read_events().map_err(|error| error.to_string())?;
        for event in events {
            if matches_ctrl_char(&event, 'c')
                || (matches!(event.code, KeyCode::Escape) && event.modifiers == KeyModifiers::NONE)
            {
                let _ = renderer.clear(&mut terminal);
                let _ = terminal.show_cursor();
                let _ = terminal.stop();
                return Ok(());
            }

            let visible_rows = build_config_browser_visible_rows(&data, search.get_value());
            selected_id = sync_config_browser_selection(&visible_rows, selected_id.as_deref());
            match event.code {
                KeyCode::Up if event.modifiers == KeyModifiers::NONE => {
                    selected_id =
                        step_config_browser_selection(&visible_rows, selected_id.as_deref(), -1);
                    continue;
                }
                KeyCode::Down if event.modifiers == KeyModifiers::NONE => {
                    selected_id =
                        step_config_browser_selection(&visible_rows, selected_id.as_deref(), 1);
                    continue;
                }
                KeyCode::Home if event.modifiers == KeyModifiers::NONE => {
                    selected_id = visible_rows.first().map(|item| item.item.id.clone());
                    continue;
                }
                KeyCode::End if event.modifiers == KeyModifiers::NONE => {
                    selected_id = visible_rows.last().map(|item| item.item.id.clone());
                    continue;
                }
                KeyCode::Enter
                    if event.modifiers == KeyModifiers::NONE
                        || event.modifiers == KeyModifiers::SHIFT =>
                {
                    if let Some(item) =
                        selected_config_browser_item(&visible_rows, selected_id.as_deref())
                    {
                        status = Some(toggle_config_browser_item(
                            &mut package_manager,
                            &data,
                            item,
                        )?);
                        selected_id = Some(item.id.clone());
                    }
                    continue;
                }
                KeyCode::Char(' ') if event.modifiers == KeyModifiers::NONE => {
                    if let Some(item) =
                        selected_config_browser_item(&visible_rows, selected_id.as_deref())
                    {
                        status = Some(toggle_config_browser_item(
                            &mut package_manager,
                            &data,
                            item,
                        )?);
                        selected_id = Some(item.id.clone());
                    }
                    continue;
                }
                _ => {}
            }

            match search.handle_key(&event) {
                InputEvent::Changed => {
                    status = None;
                    selected_id = None;
                }
                InputEvent::Cancelled => {
                    let _ = renderer.clear(&mut terminal);
                    let _ = terminal.show_cursor();
                    let _ = terminal.stop();
                    return Ok(());
                }
                InputEvent::Submitted(_) | InputEvent::None => {}
            }
        }
    }
}

fn resource_scope_from_package_scope(scope: PackageInstallScope) -> ResourceScope {
    match scope {
        PackageInstallScope::User => ResourceScope::Global,
        PackageInstallScope::Project | PackageInstallScope::Temporary => ResourceScope::Project,
    }
}

pub(super) fn build_config_browser_data(package_manager: &PackageManager) -> ConfigBrowserData {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let package_roots = package_manager
        .resource_roots()
        .into_iter()
        .map(|(scope, path)| ScopedPath {
            scope: resource_scope_from_package_scope(scope),
            path,
        })
        .collect::<Vec<_>>();
    let catalog = catalog_resources_with_options(&ResourceDiscoveryOptions {
        cwd: cwd.clone(),
        agent_dir: Some(package_manager.agent_dir().to_path_buf()),
        settings_manager: Some(package_manager.settings_manager().clone()),
        package_roots,
        ..ResourceDiscoveryOptions::default()
    });
    let packages = package_manager.list_all();
    let mut groups = Vec::new();
    append_config_browser_groups(
        &mut groups,
        &catalog,
        ConfigResourceKind::Skill,
        package_manager,
        &packages,
        package_manager.agent_dir(),
        &cwd,
    );
    append_config_browser_groups(
        &mut groups,
        &catalog,
        ConfigResourceKind::Prompt,
        package_manager,
        &packages,
        package_manager.agent_dir(),
        &cwd,
    );
    append_config_browser_groups(
        &mut groups,
        &catalog,
        ConfigResourceKind::Theme,
        package_manager,
        &packages,
        package_manager.agent_dir(),
        &cwd,
    );
    groups.sort_by(|left, right| left.title.cmp(&right.title));
    ConfigBrowserData {
        groups,
        has_extension_config: config_browser_has_extension_config(package_manager),
    }
}

fn append_config_browser_groups(
    target: &mut Vec<ConfigBrowserGroup>,
    catalog: &ResourceCatalog,
    kind: ConfigResourceKind,
    package_manager: &PackageManager,
    packages: &[InstalledPackage],
    agent_dir: &Path,
    cwd: &Path,
) {
    let groups = match kind {
        ConfigResourceKind::Skill => &catalog.skills,
        ConfigResourceKind::Prompt => &catalog.prompts,
        ConfigResourceKind::Theme => &catalog.themes,
    };

    for group in groups {
        let title = config_browser_group_title(group, packages);
        let section_index = ensure_config_browser_group_section(target, &title, kind.label());
        let items = build_config_browser_items_for_group(
            group,
            kind,
            package_manager,
            packages,
            agent_dir,
            cwd,
            &title,
        );
        target[section_index.0].sections[section_index.1]
            .items
            .extend(items);
    }
}

fn ensure_config_browser_group_section(
    groups: &mut Vec<ConfigBrowserGroup>,
    title: &str,
    section_title: &'static str,
) -> (usize, usize) {
    let group_index = if let Some(index) = groups.iter().position(|group| group.title == title) {
        index
    } else {
        groups.push(ConfigBrowserGroup {
            title: title.to_string(),
            sections: Vec::new(),
        });
        groups.len() - 1
    };
    let section_index = if let Some(index) = groups[group_index]
        .sections
        .iter()
        .position(|section| section.title == section_title)
    {
        index
    } else {
        groups[group_index].sections.push(ConfigBrowserSection {
            title: section_title,
            items: Vec::new(),
        });
        groups[group_index].sections.len() - 1
    };
    (group_index, section_index)
}

fn config_browser_group_title(
    group: &ResourceCatalogGroup,
    packages: &[InstalledPackage],
) -> String {
    match &group.origin {
        ResourceOrigin::Package { root } => {
            let source = packages
                .iter()
                .find(|package| &package.install_path == root)
                .map(|package| package.source.clone())
                .unwrap_or_else(|| shorten_home_path(&root.to_string_lossy()));
            format!(
                "{} package · {}",
                config_browser_scope_label(group.scope),
                source
            )
        }
        ResourceOrigin::TopLevel { root } => format!(
            "{} top-level · {}",
            config_browser_scope_label(group.scope),
            shorten_home_path(&root.to_string_lossy())
        ),
    }
}

fn build_config_browser_items_for_group(
    group: &ResourceCatalogGroup,
    kind: ConfigResourceKind,
    package_manager: &PackageManager,
    packages: &[InstalledPackage],
    agent_dir: &Path,
    cwd: &Path,
    group_title: &str,
) -> Vec<ConfigBrowserItem> {
    group
        .entries
        .iter()
        .map(|entry| {
            let title = config_browser_item_title(&entry.path);
            let owner = match &group.origin {
                ResourceOrigin::Package { root } => {
                    let package = packages
                        .iter()
                        .find(|package| &package.install_path == root)
                        .expect("package origin should map to installed package");
                    ConfigBrowserOwner::Package {
                        identity: package.identity.clone(),
                        scope: package.scope,
                        source: package.source.clone(),
                        install_path: package.install_path.clone(),
                    }
                }
                ResourceOrigin::TopLevel { .. } => ConfigBrowserOwner::TopLevel {
                    scope: config_browser_settings_scope(group.scope),
                    base_dir: match group.scope {
                        ResourceScope::Global => agent_dir.to_path_buf(),
                        ResourceScope::Project => get_project_config_dir(cwd),
                    },
                },
            };
            ConfigBrowserItem {
                id: format!("{}:{}", kind.settings_key(), entry.path.to_string_lossy()),
                title: title.clone(),
                path: entry.path.clone(),
                enabled: match &owner {
                    ConfigBrowserOwner::Package {
                        identity,
                        scope,
                        install_path,
                        ..
                    } => package_manager.package_resource_enabled(
                        identity,
                        *scope,
                        kind.package_resource_kind(),
                        install_path,
                        &entry.path,
                    ),
                    ConfigBrowserOwner::TopLevel { .. } => entry.enabled,
                },
                kind,
                owner,
                searchable: format!(
                    "{} {} {} {}",
                    title,
                    group_title,
                    kind.label(),
                    shorten_home_path(&entry.path.to_string_lossy())
                ),
            }
        })
        .collect()
}

fn config_browser_scope_label(scope: ResourceScope) -> &'static str {
    match scope {
        ResourceScope::Global => "Global",
        ResourceScope::Project => "Project",
    }
}

fn config_browser_settings_scope(scope: ResourceScope) -> SettingsScope {
    match scope {
        ResourceScope::Global => SettingsScope::Global,
        ResourceScope::Project => SettingsScope::Project,
    }
}

fn config_browser_item_title(path: &Path) -> String {
    path.file_stem()
        .and_then(|value| value.to_str())
        .or_else(|| path.file_name().and_then(|value| value.to_str()))
        .unwrap_or("resource")
        .to_string()
}

fn config_browser_has_extension_config(package_manager: &PackageManager) -> bool {
    for scope in [SettingsScope::Project, SettingsScope::Global] {
        if !package_manager
            .settings_manager()
            .get_string_list("extensions", Some(scope))
            .is_empty()
        {
            return true;
        }

        if package_manager
            .settings_manager()
            .scoped_settings(scope)
            .get("packages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| serde_json::from_value::<PackageConfigEntry>(entry.clone()).ok())
            .any(|entry| {
                matches!(
                    entry,
                    PackageConfigEntry::Object {
                        extensions: Some(ref extensions),
                        ..
                    } if !extensions.is_empty()
                )
            })
        {
            return true;
        }
    }
    false
}

pub(super) fn build_config_browser_visible_rows(
    data: &ConfigBrowserData,
    filter: &str,
) -> Vec<ConfigBrowserVisibleRow> {
    let filter = filter.trim().to_ascii_lowercase();
    let mut rows = Vec::new();
    for group in &data.groups {
        for section in &group.sections {
            for item in &section.items {
                if filter.is_empty() || item.searchable.to_ascii_lowercase().contains(&filter) {
                    rows.push(ConfigBrowserVisibleRow {
                        title: group.title.clone(),
                        item: item.clone(),
                    });
                }
            }
        }
    }
    rows
}

fn build_config_browser_display_rows(
    data: &ConfigBrowserData,
    filter: &str,
) -> (
    Vec<ConfigBrowserDisplayRow>,
    Vec<usize>,
    Vec<ConfigBrowserVisibleRow>,
) {
    let filter = filter.trim().to_ascii_lowercase();
    let mut rows = Vec::new();
    let mut item_row_indices = Vec::new();
    let mut visible_items = Vec::new();

    for group in &data.groups {
        let mut group_rows = Vec::new();
        let mut group_item_rows = Vec::new();
        let mut group_visible = Vec::new();
        for section in &group.sections {
            let section_items = section
                .items
                .iter()
                .filter(|item| {
                    filter.is_empty() || item.searchable.to_ascii_lowercase().contains(&filter)
                })
                .cloned()
                .collect::<Vec<_>>();
            if section_items.is_empty() {
                continue;
            }
            if group_rows.is_empty() {
                group_rows.push(ConfigBrowserDisplayRow::Group(group.title.clone()));
            }
            group_rows.push(ConfigBrowserDisplayRow::Section(section.title));
            for item in section_items {
                group_item_rows.push(rows.len() + group_rows.len());
                group_visible.push(ConfigBrowserVisibleRow {
                    title: group.title.clone(),
                    item: item.clone(),
                });
                group_rows.push(ConfigBrowserDisplayRow::Item(item));
            }
            group_rows.push(ConfigBrowserDisplayRow::Blank);
        }
        if matches!(group_rows.last(), Some(ConfigBrowserDisplayRow::Blank)) {
            group_rows.pop();
        }
        rows.extend(group_rows);
        item_row_indices.extend(group_item_rows);
        visible_items.extend(group_visible);
        if !rows.is_empty() {
            rows.push(ConfigBrowserDisplayRow::Blank);
        }
    }
    if matches!(rows.last(), Some(ConfigBrowserDisplayRow::Blank)) {
        rows.pop();
    }
    (rows, item_row_indices, visible_items)
}

pub(super) fn sync_config_browser_selection(
    visible_rows: &[ConfigBrowserVisibleRow],
    selected_id: Option<&str>,
) -> Option<String> {
    if visible_rows.is_empty() {
        return None;
    }
    selected_id
        .filter(|selected_id| visible_rows.iter().any(|row| row.item.id == *selected_id))
        .map(ToOwned::to_owned)
        .or_else(|| visible_rows.first().map(|row| row.item.id.clone()))
}

pub(super) fn step_config_browser_selection(
    visible_rows: &[ConfigBrowserVisibleRow],
    selected_id: Option<&str>,
    delta: isize,
) -> Option<String> {
    if visible_rows.is_empty() {
        return None;
    }
    let current_index = selected_id
        .and_then(|selected_id| {
            visible_rows
                .iter()
                .position(|row| row.item.id == selected_id)
        })
        .unwrap_or(0);
    let next_index = (current_index as isize + delta)
        .clamp(0, visible_rows.len().saturating_sub(1) as isize) as usize;
    Some(visible_rows[next_index].item.id.clone())
}

pub(super) fn selected_config_browser_item<'a>(
    visible_rows: &'a [ConfigBrowserVisibleRow],
    selected_id: Option<&str>,
) -> Option<&'a ConfigBrowserItem> {
    let selected_id = selected_id?;
    visible_rows
        .iter()
        .find(|row| row.item.id == selected_id)
        .map(|row| &row.item)
}

pub(super) fn render_config_browser(
    search: &Input,
    data: &ConfigBrowserData,
    visible_rows: &[ConfigBrowserVisibleRow],
    selected_id: Option<&str>,
    status: Option<&str>,
    width: u16,
    height: u16,
) -> RenderOutput {
    let mut output = RenderOutput::default();
    append_overlay_banner(
        &mut output,
        "Resource Configuration",
        "Type to filter resources",
        width,
    );
    append_blank_lines(&mut output, width, 1);
    append_output(&mut output, search.render(width), true);
    append_blank_lines(&mut output, width, 1);

    let (display_rows, item_row_indices, _) =
        build_config_browser_display_rows(data, search.get_value());
    let selected_row_index = selected_id
        .and_then(|selected_id| {
            visible_rows
                .iter()
                .position(|row| row.item.id == selected_id)
                .and_then(|index| item_row_indices.get(index).copied())
        })
        .unwrap_or(0);
    let selected_item = selected_config_browser_item(visible_rows, selected_id);

    let mut footer_lines = 2usize;
    if status.is_some() {
        footer_lines += 2;
    }
    if selected_item.is_some() {
        footer_lines += 2;
    }
    if data.has_extension_config {
        footer_lines += 1;
    }
    let available_rows = height
        .saturating_sub(output.lines.len() as u16)
        .saturating_sub(footer_lines as u16) as usize;

    if display_rows.is_empty() {
        append_output(
            &mut output,
            Text::new(style_hint("No resources found")).render(width),
            false,
        );
    } else {
        let (start, end) = config_browser_window(&display_rows, available_rows, selected_row_index);
        for row in &display_rows[start..end] {
            let rendered = match row {
                ConfigBrowserDisplayRow::Group(title) => style_title(title),
                ConfigBrowserDisplayRow::Section(title) => style_subtitle(title),
                ConfigBrowserDisplayRow::Item(item) => {
                    let checkbox = if item.enabled { "[x]" } else { "[ ]" };
                    let text =
                        truncate_to_width(&format!("{checkbox} {}", item.title), width as usize);
                    if selected_id.is_some_and(|selected_id| selected_id == item.id) {
                        style_selected_row(&text)
                    } else {
                        text
                    }
                }
                ConfigBrowserDisplayRow::Blank => String::new(),
            };
            append_output(&mut output, Text::new(rendered).render(width), false);
        }
    }

    if let Some(item) = selected_item {
        append_blank_lines(&mut output, width, 1);
        append_output(
            &mut output,
            Text::new(style_hint(&shorten_home_path(&item.path.to_string_lossy()))).render(width),
            false,
        );
    }

    if let Some(status) = status {
        append_blank_lines(&mut output, width, 1);
        append_output(
            &mut output,
            Text::new(style_subtitle(status)).render(width),
            false,
        );
    }

    append_blank_lines(&mut output, width, 1);
    append_output(
        &mut output,
        Text::new(style_hint("Space/Enter toggle · Esc closes")).render(width),
        false,
    );
    if data.has_extension_config {
        append_output(
            &mut output,
            Text::new(style_hint(
                "Extensions stay static until the Rust plugin host lands.",
            ))
            .render(width),
            false,
        );
    }

    output
}

fn config_browser_window(
    rows: &[ConfigBrowserDisplayRow],
    available_rows: usize,
    selected_row_index: usize,
) -> (usize, usize) {
    if available_rows == 0 || rows.len() <= available_rows {
        return (0, rows.len());
    }
    let mut start = selected_row_index.saturating_sub(available_rows / 2);
    let mut end = start + available_rows;
    if end > rows.len() {
        end = rows.len();
        start = end.saturating_sub(available_rows);
    }
    while start > 0
        && matches!(
            rows[start],
            ConfigBrowserDisplayRow::Item(_) | ConfigBrowserDisplayRow::Blank
        )
    {
        start -= 1;
        if end.saturating_sub(start) > available_rows {
            end = end.saturating_sub(1);
        }
    }
    (start, end)
}

pub(super) fn toggle_config_browser_item(
    package_manager: &mut PackageManager,
    data: &ConfigBrowserData,
    item: &ConfigBrowserItem,
) -> Result<String, String> {
    match &item.owner {
        ConfigBrowserOwner::TopLevel { scope, base_dir } => {
            toggle_top_level_resource(package_manager, *scope, base_dir, item)?;
        }
        ConfigBrowserOwner::Package {
            identity,
            scope,
            source,
            install_path,
        } => {
            toggle_package_resource(
                package_manager,
                data,
                identity,
                *scope,
                source,
                install_path,
                item,
            )?;
        }
    }

    Ok(format!(
        "{} {}",
        if item.enabled { "Disabled" } else { "Enabled" },
        shorten_home_path(&item.path.to_string_lossy())
    ))
}

fn toggle_top_level_resource(
    package_manager: &mut PackageManager,
    scope: SettingsScope,
    base_dir: &Path,
    item: &ConfigBrowserItem,
) -> Result<(), String> {
    pi_rust_resources::toggle_scoped_resource_entry(
        package_manager.settings_manager_mut(),
        scope,
        item.kind.resource_kind(),
        config_browser_settings_path(base_dir, &item.path),
        !item.enabled,
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn toggle_package_resource(
    package_manager: &mut PackageManager,
    data: &ConfigBrowserData,
    identity: &str,
    scope: PackageInstallScope,
    source: &str,
    install_path: &Path,
    item: &ConfigBrowserItem,
) -> Result<(), String> {
    let _ = config_browser_group_items(data, scope, source, install_path, item.kind);
    package_manager
        .set_package_resource_enabled(
            identity,
            scope,
            item.kind.package_resource_kind(),
            install_path,
            &item.path,
            !item.enabled,
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn config_browser_group_items(
    data: &ConfigBrowserData,
    scope: PackageInstallScope,
    source: &str,
    install_path: &Path,
    kind: ConfigResourceKind,
) -> Vec<ConfigBrowserItem> {
    data.groups
        .iter()
        .flat_map(|group| group.sections.iter())
        .filter(|section| section.title == kind.label())
        .flat_map(|section| section.items.iter())
        .filter(|item| {
            matches!(
                &item.owner,
                ConfigBrowserOwner::Package {
                    identity: _,
                    scope: item_scope,
                    source: item_source,
                    install_path: item_root,
                } if item_scope == &scope && item_source == source && item_root == install_path
            ) && item.kind == kind
        })
        .cloned()
        .collect()
}

fn config_browser_settings_path(base_dir: &Path, path: &Path) -> String {
    if let Ok(relative) = path.strip_prefix(base_dir) {
        return normalize_config_browser_path(relative.to_string_lossy().as_ref());
    }
    normalize_config_browser_path(&path.to_string_lossy())
}

fn normalize_config_browser_path(value: &str) -> String {
    value.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use tempfile::tempdir;

    fn env_guard() -> &'static Mutex<()> {
        crate::test_env_guard()
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, content).expect("write file");
    }

    fn with_current_dir<T>(cwd: &Path, f: impl FnOnce() -> T) -> T {
        struct CwdGuard(PathBuf);

        impl Drop for CwdGuard {
            fn drop(&mut self) {
                std::env::set_current_dir(&self.0).expect("restore cwd");
            }
        }

        let original = std::env::current_dir().expect("current dir");
        let _cwd_guard = CwdGuard(original);
        std::env::set_current_dir(cwd).expect("set cwd");
        f()
    }

    #[test]
    fn config_browser_visible_rows_include_top_level_and_package_resources() {
        let _guard = env_guard().lock().expect("lock");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        let package_dir = tempdir.path().join("local-package");
        std::fs::create_dir_all(&cwd).expect("cwd");
        write_file(&agent_dir.join("skills").join("global.md"), "global");
        write_file(
            &cwd.join(".pi").join("prompts").join("project.md"),
            "project",
        );
        write_file(&package_dir.join("SYSTEM.md"), "system");
        write_file(&package_dir.join("skills").join("pkg.md"), "pkg");
        let mut manager = PackageManager::create(&cwd, Some(agent_dir.clone()));
        manager
            .install(
                package_dir.to_string_lossy().as_ref(),
                PackageInstallScope::Project,
            )
            .expect("install package");

        let data = with_current_dir(&cwd, || build_config_browser_data(&manager));
        let rows = build_config_browser_visible_rows(&data, "");

        assert!(rows.iter().any(|row| row.item.title == "global"));
        assert!(rows.iter().any(|row| row.item.title == "project"));
        assert!(rows.iter().any(|row| row.item.title == "pkg"));
        assert!(
            rows.iter()
                .any(|row| matches!(row.item.owner, ConfigBrowserOwner::Package { .. }))
        );
    }

    #[test]
    fn toggling_config_browser_item_persists_after_rebuild() {
        let _guard = env_guard().lock().expect("lock");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        std::fs::create_dir_all(&cwd).expect("cwd");
        write_file(&agent_dir.join("skills").join("global.md"), "global");
        let mut manager = PackageManager::create(&cwd, Some(agent_dir.clone()));

        let initial_data = with_current_dir(&cwd, || build_config_browser_data(&manager));
        let initial_rows = build_config_browser_visible_rows(&initial_data, "global");
        let item = initial_rows
            .iter()
            .find(|row| row.item.title == "global")
            .expect("global row")
            .item
            .clone();
        assert!(item.enabled);

        with_current_dir(&cwd, || {
            toggle_config_browser_item(&mut manager, &initial_data, &item)
        })
        .expect("toggle resource");

        let rebuilt = with_current_dir(&cwd, || build_config_browser_data(&manager));
        let rebuilt_rows = build_config_browser_visible_rows(&rebuilt, "global");
        let rebuilt_item = rebuilt_rows
            .iter()
            .find(|row| row.item.title == "global")
            .expect("rebuilt row");
        assert!(!rebuilt_item.item.enabled);

        let settings_path = agent_dir.join("settings.json");
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(settings_path).expect("settings"))
                .expect("parse settings");
        assert_eq!(saved["skills"], serde_json::json!(["-skills/global.md"]));

        let reenabled_item = rebuilt_item.item.clone();
        with_current_dir(&cwd, || {
            toggle_config_browser_item(&mut manager, &rebuilt, &reenabled_item)
        })
        .expect("re-enable resource");

        let rebuilt_again = with_current_dir(&cwd, || build_config_browser_data(&manager));
        let rebuilt_again_rows = build_config_browser_visible_rows(&rebuilt_again, "global");
        let rebuilt_again_item = rebuilt_again_rows
            .iter()
            .find(|row| row.item.title == "global")
            .expect("rebuilt row");
        assert!(rebuilt_again_item.item.enabled);

        let saved: Value = serde_json::from_str(
            &std::fs::read_to_string(agent_dir.join("settings.json")).expect("settings"),
        )
        .expect("parse settings");
        assert_eq!(saved["skills"], serde_json::json!(["+skills/global.md"]));
    }

    #[test]
    fn toggling_disabled_package_resource_persists_exact_reenable() {
        let _guard = env_guard().lock().expect("lock");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        let package_dir = tempdir.path().join("local-package");
        std::fs::create_dir_all(&cwd).expect("cwd");
        write_file(&package_dir.join("SYSTEM.md"), "system");
        write_file(&package_dir.join("skills").join("core.md"), "core");
        write_file(&package_dir.join("skills").join("keep.md"), "keep");
        let mut manager = PackageManager::create(&cwd, Some(agent_dir.clone()));
        let installed = manager
            .install(
                package_dir.to_string_lossy().as_ref(),
                PackageInstallScope::Project,
            )
            .expect("install package");
        manager
            .set_package_filters(
                &installed.identity,
                PackageInstallScope::Project,
                pi_rust_packages::PackageResourceKind::Skills,
                Some(&["skills/core.md".to_string()]),
            )
            .expect("seed package filters");

        let initial_data = with_current_dir(&cwd, || build_config_browser_data(&manager));
        let initial_rows = build_config_browser_visible_rows(&initial_data, "keep");
        let item = initial_rows
            .iter()
            .find(|row| row.item.title == "keep")
            .expect("keep row")
            .item
            .clone();
        assert!(!item.enabled);

        with_current_dir(&cwd, || {
            toggle_config_browser_item(&mut manager, &initial_data, &item)
        })
        .expect("toggle package resource");

        let rebuilt = with_current_dir(&cwd, || build_config_browser_data(&manager));
        let rebuilt_rows = build_config_browser_visible_rows(&rebuilt, "keep");
        let rebuilt_item = rebuilt_rows
            .iter()
            .find(|row| row.item.title == "keep")
            .expect("rebuilt row");
        assert!(rebuilt_item.item.enabled);

        let saved: Value = serde_json::from_str(
            &std::fs::read_to_string(cwd.join(".pi").join("settings.json")).expect("settings"),
        )
        .expect("parse settings");
        assert_eq!(
            saved["packages"][0]["skills"],
            serde_json::json!(["skills/core.md", "+skills/keep.md"])
        );
    }

    #[test]
    fn toggling_package_resource_with_mixed_filters_removes_stale_exact_entries() {
        let _guard = env_guard().lock().expect("lock");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        let package_dir = tempdir.path().join("local-package");
        std::fs::create_dir_all(&cwd).expect("cwd");
        write_file(&package_dir.join("SYSTEM.md"), "system");
        write_file(&package_dir.join("skills").join("core.md"), "core");
        write_file(&package_dir.join("skills").join("off.md"), "off");
        write_file(&package_dir.join("skills").join("keep.md"), "keep");
        let mut manager = PackageManager::create(&cwd, Some(agent_dir.clone()));
        let installed = manager
            .install(
                package_dir.to_string_lossy().as_ref(),
                PackageInstallScope::Project,
            )
            .expect("install package");
        manager
            .set_package_filters(
                &installed.identity,
                PackageInstallScope::Project,
                pi_rust_packages::PackageResourceKind::Skills,
                Some(&[
                    "skills/*.md".to_string(),
                    "+skills/keep.md".to_string(),
                    "-skills/off.md".to_string(),
                ]),
            )
            .expect("seed mixed filters");

        let initial_data = with_current_dir(&cwd, || build_config_browser_data(&manager));
        let off_item = build_config_browser_visible_rows(&initial_data, "off")
            .into_iter()
            .find(|row| row.item.title == "off")
            .expect("off row")
            .item;
        assert!(!off_item.enabled);

        with_current_dir(&cwd, || {
            toggle_config_browser_item(&mut manager, &initial_data, &off_item)
        })
        .expect("toggle off item");

        let rebuilt = with_current_dir(&cwd, || build_config_browser_data(&manager));
        let off_rebuilt = build_config_browser_visible_rows(&rebuilt, "off")
            .into_iter()
            .find(|row| row.item.title == "off")
            .expect("rebuilt off row")
            .item;
        assert!(off_rebuilt.enabled);

        let saved: Value = serde_json::from_str(
            &std::fs::read_to_string(cwd.join(".pi").join("settings.json")).expect("settings"),
        )
        .expect("parse settings");
        assert_eq!(
            saved["packages"][0]["skills"],
            serde_json::json!(["skills/*.md"])
        );

        let keep_item = build_config_browser_visible_rows(&rebuilt, "keep")
            .into_iter()
            .find(|row| row.item.title == "keep")
            .expect("keep row")
            .item;
        assert!(keep_item.enabled);

        with_current_dir(&cwd, || {
            toggle_config_browser_item(&mut manager, &rebuilt, &keep_item)
        })
        .expect("toggle keep item");

        let rebuilt_again = with_current_dir(&cwd, || build_config_browser_data(&manager));
        let keep_rebuilt = build_config_browser_visible_rows(&rebuilt_again, "keep")
            .into_iter()
            .find(|row| row.item.title == "keep")
            .expect("rebuilt keep row")
            .item;
        assert!(!keep_rebuilt.enabled);

        let saved: Value = serde_json::from_str(
            &std::fs::read_to_string(cwd.join(".pi").join("settings.json")).expect("settings"),
        )
        .expect("parse settings");
        assert_eq!(
            saved["packages"][0]["skills"],
            serde_json::json!(["skills/*.md", "-skills/keep.md"])
        );
    }
}
