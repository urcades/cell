use super::super::*;
use super::base::{OverlaySelection, SearchOverlay, select_list_visible_bounds};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelOverlayScope {
    All,
    Scoped,
}

impl ModelOverlayScope {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Scoped => "scoped",
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::All => Self::Scoped,
            Self::Scoped => Self::All,
        }
    }
}

pub(crate) struct ModelOverlayState {
    pub(crate) overlay: SearchOverlay,
    pub(crate) selections: Vec<OverlaySelection>,
    pub(crate) models: Vec<Model>,
    pub(crate) current_model: Option<Model>,
    pub(crate) scope: ModelOverlayScope,
    pub(crate) available_count: usize,
    pub(crate) scoped_count: usize,
}

pub(crate) struct ScopedModelsOverlayState {
    pub(crate) overlay: SearchOverlay,
    pub(crate) models: Vec<Model>,
    pub(crate) enabled_ids: Option<Vec<String>>,
    pub(crate) dirty: bool,
}

impl Component for ModelOverlayState {
    fn render(&self, width: u16) -> RenderOutput {
        let mut output = RenderOutput {
            lines: Vec::new(),
            cursor: None,
        };
        append_rule_line(&mut output.lines, width);
        append_blank_lines(&mut output, width, 1);
        append_output(
            &mut output,
            Text::new(model_overlay_scope_line(self)).render(width),
            false,
        );
        if self.scoped_count > 0 {
            append_output(
                &mut output,
                Text::new(style_hint("Tab scope (all/scoped)")).render(width),
                false,
            );
        }
        append_blank_lines(&mut output, width, 1);
        append_output(&mut output, self.overlay.search.render(width), false);
        append_blank_lines(&mut output, width, 1);
        append_model_overlay_rows(&mut output.lines, self, width as usize);
        if let Some(model) = selected_model_overlay_model(self) {
            append_blank_lines(&mut output, width, 1);
            append_output(
                &mut output,
                Text::new(style_hint(&format!("  Model Name: {}", model.name))).render(width),
                false,
            );
        }
        append_blank_lines(&mut output, width, 1);
        append_rule_line(&mut output.lines, width);
        output
    }
}

impl Component for ScopedModelsOverlayState {
    fn render(&self, width: u16) -> RenderOutput {
        let mut output = RenderOutput::default();
        append_rule_line(&mut output.lines, width);
        append_blank_lines(&mut output, width, 1);
        append_output(
            &mut output,
            Text::new(style_title("Model Configuration")).render(width),
            false,
        );
        append_output(
            &mut output,
            Text::new(style_subtitle("Session-only. Ctrl+S to save to settings.")).render(width),
            false,
        );
        append_blank_lines(&mut output, width, 1);
        append_output(&mut output, self.overlay.search.render(width), false);
        append_blank_lines(&mut output, width, 1);
        append_scoped_models_overlay_rows(&mut output.lines, self, width as usize);
        append_blank_lines(&mut output, width, 1);
        append_output(
            &mut output,
            Text::new(style_hint(&scoped_models_footer_text(self))).render(width),
            false,
        );
        append_blank_lines(&mut output, width, 1);
        append_rule_line(&mut output.lines, width);
        output
    }
}

pub(crate) fn build_model_overlay_items(
    models: &[Model],
    current_model: Option<&Model>,
) -> (Vec<SelectItem>, Vec<OverlaySelection>) {
    let models = sort_model_overlay_models(models, current_model);

    let items = models
        .iter()
        .map(|model| {
            let is_current = current_model
                .map(|current| current.provider == model.provider && current.id == model.id)
                .unwrap_or(false);
            let label = format!(
                "{} [{}]{}",
                model.id,
                model.provider.0,
                if is_current { " ✓" } else { "" }
            );
            let description = format!(
                "{} · {} · {} ctx",
                model.name,
                if model.reasoning { "reasoning" } else { "text" },
                format_token_count(model.context_window as u64)
            );
            SelectItem {
                value: format!("{}/{}", model.provider.0, model.id),
                label,
                description: Some(description),
            }
        })
        .collect::<Vec<_>>();
    let selections = models
        .iter()
        .map(|model| OverlaySelection::Model {
            provider: model.provider.0.clone(),
            model_id: model.id.clone(),
        })
        .collect::<Vec<_>>();
    (items, selections)
}

pub(crate) fn update_model_overlay_metadata(
    overlay: &mut SearchOverlay,
    available_count: usize,
    scoped_count: usize,
    current_model: Option<&Model>,
    scope: ModelOverlayScope,
) {
    overlay.set_title("Model Selector");
    let current = current_model
        .map(|model| format!("{}/{}", model.provider.0, model.id))
        .unwrap_or_else(|| "no-model".to_string());
    let selected = overlay
        .selected_item()
        .map(|item| truncate_to_width(&item.label.replace('\n', " "), 48));
    let detail = overlay
        .selected_item()
        .and_then(|item| item.description.clone());
    let subtitle = if scoped_count > 0 {
        format!(
            "scope {} · {} all · {} scoped\ncurrent {}{}",
            scope.label(),
            available_count,
            scoped_count,
            current,
            selected
                .map(|value| format!(" · {value}"))
                .unwrap_or_default()
        )
    } else {
        format!(
            "{} available\ncurrent {}{}",
            available_count,
            current,
            selected
                .map(|value| format!(" · {value}"))
                .unwrap_or_default()
        )
    };
    overlay.set_subtitle(subtitle);
    overlay.set_detail(detail);
    overlay.set_hint(if scoped_count > 0 {
        "Tab toggles scope · Enter selects · Search filters id/provider/name · Esc cancels"
    } else {
        "Enter selects · Search filters id/provider/name · Esc cancels"
    });
}

pub(crate) fn sort_model_overlay_models(
    models: &[Model],
    current_model: Option<&Model>,
) -> Vec<Model> {
    let mut models = models.to_vec();
    models.sort_by(|left, right| {
        let left_current = current_model
            .map(|current| current.provider == left.provider && current.id == left.id)
            .unwrap_or(false);
        let right_current = current_model
            .map(|current| current.provider == right.provider && current.id == right.id)
            .unwrap_or(false);
        right_current
            .cmp(&left_current)
            .then(left.provider.0.cmp(&right.provider.0))
            .then(left.id.cmp(&right.id))
    });
    models
}

pub(crate) fn model_full_id(model: &Model) -> String {
    format!("{}/{}", model.provider.0, model.id)
}

pub(crate) fn build_scoped_model_items(
    models: &[Model],
    enabled_ids: Option<&[String]>,
) -> Vec<SelectItem> {
    models
        .iter()
        .map(|model| {
            let full_id = model_full_id(model);
            let enabled = enabled_ids
                .map(|ids| ids.iter().any(|id| id == &full_id))
                .unwrap_or(true);
            SelectItem {
                value: full_id,
                label: format!(
                    "{} [{}]{}",
                    model.id,
                    model.provider.0,
                    if enabled { " ✓" } else { " ✗" }
                ),
                description: Some(model.name.clone()),
            }
        })
        .collect()
}

pub(crate) fn toggle_scoped_model(state: &mut ScopedModelsOverlayState, model_id: &str) {
    match state.enabled_ids.as_mut() {
        None => {
            state.enabled_ids = Some(vec![model_id.to_string()]);
        }
        Some(enabled_ids) => {
            if let Some(index) = enabled_ids.iter().position(|id| id == model_id) {
                enabled_ids.remove(index);
            } else {
                enabled_ids.push(model_id.to_string());
            }
        }
    }
}

pub(crate) fn toggle_scoped_models_provider(
    state: &mut ScopedModelsOverlayState,
    selected_value: &str,
) {
    let Some(selected_model) = state
        .models
        .iter()
        .find(|model| model_full_id(model) == selected_value)
    else {
        return;
    };
    let provider = selected_model.provider.0.as_str();
    let provider_ids = state
        .models
        .iter()
        .filter(|model| model.provider.0 == provider)
        .map(model_full_id)
        .collect::<Vec<_>>();
    let all_provider_enabled = provider_ids.iter().all(|id| {
        state
            .enabled_ids
            .as_ref()
            .is_none_or(|ids| ids.iter().any(|value| value == id))
    });
    if all_provider_enabled {
        let mut next = state
            .enabled_ids
            .clone()
            .unwrap_or_else(|| state.models.iter().map(model_full_id).collect());
        next.retain(|id| !provider_ids.iter().any(|provider_id| provider_id == id));
        state.enabled_ids = Some(next);
    } else {
        let mut next = state.enabled_ids.clone().unwrap_or_default();
        for id in provider_ids {
            if !next.iter().any(|existing| existing == &id) {
                next.push(id);
            }
        }
        if next.len() == state.models.len() {
            state.enabled_ids = None;
        } else {
            state.enabled_ids = Some(next);
        }
    }
}

pub(crate) fn move_scoped_model_selection(
    state: &mut ScopedModelsOverlayState,
    selected_value: &str,
    delta: isize,
) -> bool {
    let enabled_ids = state
        .enabled_ids
        .clone()
        .unwrap_or_else(|| state.models.iter().map(model_full_id).collect::<Vec<_>>());
    let Some(index) = enabled_ids.iter().position(|id| id == selected_value) else {
        return false;
    };
    let next_index = index as isize + delta;
    if next_index < 0 || next_index >= enabled_ids.len() as isize {
        return false;
    }
    let mut next = enabled_ids;
    next.swap(index, next_index as usize);
    state.enabled_ids = Some(next);
    true
}

pub(crate) fn model_overlay_scope_line(state: &ModelOverlayState) -> String {
    if state.scoped_count == 0 {
        return style_warning(
            "Only showing models with configured API keys (see README for details)",
        );
    }
    format!(
        "{}{}{}{}",
        style_hint("Scope: "),
        if matches!(state.scope, ModelOverlayScope::All) {
            style_brand("all")
        } else {
            style_hint("all")
        },
        style_hint(" | "),
        if matches!(state.scope, ModelOverlayScope::Scoped) {
            style_brand("scoped")
        } else {
            style_hint("scoped")
        }
    )
}

pub(crate) fn selected_model_overlay_model(state: &ModelOverlayState) -> Option<&Model> {
    state
        .overlay
        .list
        .filtered_indices()
        .get(state.overlay.list.selected_index())
        .and_then(|index| state.models.get(*index))
}

pub(crate) fn append_model_overlay_rows(
    target: &mut Vec<RenderedLine>,
    state: &ModelOverlayState,
    width: usize,
) {
    let filtered_indices = state.overlay.list.filtered_indices();
    if filtered_indices.is_empty() {
        target.push(RenderedLine::Text(fit_line(
            &style_hint("  No matching models"),
            width as u16,
        )));
        return;
    }

    let selected_index = state.overlay.list.selected_index();
    let (start, end) = select_list_visible_bounds(&state.overlay.list);
    for visible_index in start..end {
        let Some(model) = filtered_indices
            .get(visible_index)
            .and_then(|index| state.models.get(*index))
        else {
            continue;
        };
        let is_selected = visible_index == selected_index;
        let is_current = state
            .current_model
            .as_ref()
            .is_some_and(|current| current.provider == model.provider && current.id == model.id);
        let prefix = if is_selected {
            style_brand("→ ")
        } else {
            "  ".to_string()
        };
        let available = width
            .saturating_sub(visible_width(&prefix))
            .saturating_sub(visible_width(" [] ✓"));
        let model_id = truncate_to_width(&model.id, available.max(16));
        let model_text = if is_selected {
            style_brand(&model_id)
        } else {
            model_id
        };
        let provider_badge = style_hint(&format!("[{}]", model.provider.0));
        let checkmark = if is_current {
            style_success(" ✓")
        } else {
            String::new()
        };
        target.push(RenderedLine::Text(fit_line(
            &format!("{prefix}{model_text} {provider_badge}{checkmark}"),
            width as u16,
        )));
    }

    if start > 0 || end < filtered_indices.len() {
        target.push(RenderedLine::Text(fit_line(
            &style_hint(&format!(
                "  ({}/{})",
                selected_index + 1,
                filtered_indices.len()
            )),
            width as u16,
        )));
    }
}

pub(crate) fn append_scoped_models_overlay_rows(
    target: &mut Vec<RenderedLine>,
    state: &ScopedModelsOverlayState,
    width: usize,
) {
    let filtered_indices = state.overlay.list.filtered_indices();
    if filtered_indices.is_empty() {
        target.push(RenderedLine::Text(fit_line(
            &style_hint("  No matching models"),
            width as u16,
        )));
        return;
    }

    let selected_index = state.overlay.list.selected_index();
    let (start, end) = select_list_visible_bounds(&state.overlay.list);
    let all_enabled = state.enabled_ids.is_none();

    for visible_index in start..end {
        let Some(model) = filtered_indices
            .get(visible_index)
            .and_then(|index| state.models.get(*index))
        else {
            continue;
        };
        let is_selected = visible_index == selected_index;
        let full_id = model_full_id(model);
        let enabled = state
            .enabled_ids
            .as_ref()
            .map(|ids| ids.iter().any(|id| id == &full_id))
            .unwrap_or(true);
        let prefix = if is_selected {
            style_brand("→ ")
        } else {
            "  ".to_string()
        };
        let model_text = if is_selected {
            style_brand(&model.id)
        } else {
            model.id.clone()
        };
        let provider_badge = style_hint(&format!("[{}]", model.provider.0));
        let status = if all_enabled {
            String::new()
        } else if enabled {
            style_success(" ✓")
        } else {
            style_hint(" ✗")
        };
        target.push(RenderedLine::Text(fit_line(
            &format!("{prefix}{model_text} {provider_badge}{status}"),
            width as u16,
        )));
    }

    if start > 0 || end < filtered_indices.len() {
        target.push(RenderedLine::Text(fit_line(
            &style_hint(&format!(
                "  ({}/{})",
                selected_index + 1,
                filtered_indices.len()
            )),
            width as u16,
        )));
    }

    if let Some(model) = filtered_indices
        .get(selected_index)
        .and_then(|index| state.models.get(*index))
    {
        target.push(RenderedLine::Text(String::new()));
        target.push(RenderedLine::Text(fit_line(
            &style_hint(&format!("  Model Name: {}", model.name)),
            width as u16,
        )));
    }
}

pub(crate) fn scoped_models_footer_text(state: &ScopedModelsOverlayState) -> String {
    let enabled_count = state
        .enabled_ids
        .as_ref()
        .map_or(state.models.len(), Vec::len);
    let count_text = if state.enabled_ids.is_none() {
        "all enabled".to_string()
    } else {
        format!("{enabled_count}/{} enabled", state.models.len())
    };
    let base = format!(
        "  Enter toggle · Ctrl+A all · Ctrl+X clear · Ctrl+P provider · Alt+Up/Down reorder · Ctrl+S save · {count_text}"
    );
    if state.dirty {
        format!("{base} {}", style_warning("(unsaved)"))
    } else {
        base
    }
}
