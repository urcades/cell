use super::{PackageCommand, PluginsCommand};

pub fn render_help_text() -> &'static str {
    include_str!("../../../fixtures/cli/help.txt")
}

pub fn render_version_text() -> &'static str {
    include_str!("../../../fixtures/cli/version.txt").trim_end()
}

pub fn render_package_command_usage(command: PackageCommand) -> &'static str {
    match command {
        PackageCommand::Install => "cell install <source> [-l]",
        PackageCommand::Remove => "cell remove <source> [-l]",
        PackageCommand::Update => "cell update [source]",
        PackageCommand::List => "cell list",
    }
}

pub fn render_package_command_help(command: PackageCommand) -> &'static str {
    match command {
        PackageCommand::Install => {
            "Usage:\n  cell install <source> [-l]\n\nInstall a package and add it to settings.\n\nOptions:\n  -l, --local    Install project-locally (.pi/settings.json)\n\nExamples:\n  cell install npm:@foo/bar\n  cell install git:github.com/user/repo\n  cell install git:git@github.com:user/repo\n  cell install https://github.com/user/repo\n  cell install ssh://git@github.com/user/repo\n  cell install ./local/path\n"
        }
        PackageCommand::Remove => {
            "Usage:\n  cell remove <source> [-l]\n\nRemove a package and its source from settings.\n\nOptions:\n  -l, --local    Remove from project settings (.pi/settings.json)\n\nExample:\n  cell remove npm:@foo/bar\n"
        }
        PackageCommand::Update => {
            "Usage:\n  cell update [source]\n\nUpdate installed packages.\nIf <source> is provided, only that package is updated.\n"
        }
        PackageCommand::List => {
            "Usage:\n  cell list\n\nList installed packages from user and project settings.\n"
        }
    }
}

pub fn render_plugins_command_usage(command: PluginsCommand) -> &'static str {
    match command {
        PluginsCommand::List => "cell plugins list",
        PluginsCommand::AddRoot => "cell plugins add-root <path> [-l|--project]",
        PluginsCommand::RemoveRoot => "cell plugins remove-root <path> [-l|--project]",
    }
}

pub fn render_plugins_command_help(command: PluginsCommand) -> &'static str {
    match command {
        PluginsCommand::List => {
            "Usage:\n  cell plugins list\n\nList plugin runtime diagnostics from discovered plugins.\nUse --mode json for machine-readable diagnostics.\n"
        }
        PluginsCommand::AddRoot => {
            "Usage:\n  cell plugins add-root <path> [-l|--project]\n\nAdd a plugin root to settings.\nUse -l, --project, or --local to store the root in project settings (.pi/settings.json).\n"
        }
        PluginsCommand::RemoveRoot => {
            "Usage:\n  cell plugins remove-root <path> [-l|--project]\n\nRemove a plugin root from settings.\nUse -l, --project, or --local to target project settings (.pi/settings.json).\n"
        }
    }
}

pub fn render_plugins_help_text() -> &'static str {
    "Usage:\n  cell plugins list\n  cell plugins add-root <path> [-l|--project]\n  cell plugins remove-root <path> [-l|--project]\n\nManage plugin discovery roots and inspect plugin runtime diagnostics.\n"
}
