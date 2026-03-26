use super::{PackageCommand, PluginsCommand};

pub fn render_help_text() -> &'static str {
    include_str!("../../../fixtures/cli/help.txt")
}

pub fn render_version_text() -> &'static str {
    include_str!("../../../fixtures/cli/version.txt").trim_end()
}

pub fn render_package_command_usage(command: PackageCommand) -> &'static str {
    match command {
        PackageCommand::Install => "pi-rust install <source> [-l]",
        PackageCommand::Remove => "pi-rust remove <source> [-l]",
        PackageCommand::Update => "pi-rust update [source]",
        PackageCommand::List => "pi-rust list",
    }
}

pub fn render_package_command_help(command: PackageCommand) -> &'static str {
    match command {
        PackageCommand::Install => {
            "Usage:\n  pi-rust install <source> [-l]\n\nInstall a package and add it to settings.\n\nOptions:\n  -l, --local    Install project-locally (.pi/settings.json)\n\nExamples:\n  pi-rust install npm:@foo/bar\n  pi-rust install git:github.com/user/repo\n  pi-rust install git:git@github.com:user/repo\n  pi-rust install https://github.com/user/repo\n  pi-rust install ssh://git@github.com/user/repo\n  pi-rust install ./local/path\n"
        }
        PackageCommand::Remove => {
            "Usage:\n  pi-rust remove <source> [-l]\n\nRemove a package and its source from settings.\n\nOptions:\n  -l, --local    Remove from project settings (.pi/settings.json)\n\nExample:\n  pi-rust remove npm:@foo/bar\n"
        }
        PackageCommand::Update => {
            "Usage:\n  pi-rust update [source]\n\nUpdate installed packages.\nIf <source> is provided, only that package is updated.\n"
        }
        PackageCommand::List => {
            "Usage:\n  pi-rust list\n\nList installed packages from user and project settings.\n"
        }
    }
}

pub fn render_plugins_command_usage(command: PluginsCommand) -> &'static str {
    match command {
        PluginsCommand::List => "pi-rust plugins list",
        PluginsCommand::AddRoot => "pi-rust plugins add-root <path> [-l|--project]",
        PluginsCommand::RemoveRoot => "pi-rust plugins remove-root <path> [-l|--project]",
    }
}

pub fn render_plugins_command_help(command: PluginsCommand) -> &'static str {
    match command {
        PluginsCommand::List => {
            "Usage:\n  pi-rust plugins list\n\nList plugin runtime diagnostics from discovered plugins.\nUse --mode json for machine-readable diagnostics.\n"
        }
        PluginsCommand::AddRoot => {
            "Usage:\n  pi-rust plugins add-root <path> [-l|--project]\n\nAdd a plugin root to settings.\nUse -l, --project, or --local to store the root in project settings (.pi/settings.json).\n"
        }
        PluginsCommand::RemoveRoot => {
            "Usage:\n  pi-rust plugins remove-root <path> [-l|--project]\n\nRemove a plugin root from settings.\nUse -l, --project, or --local to target project settings (.pi/settings.json).\n"
        }
    }
}

pub fn render_plugins_help_text() -> &'static str {
    "Usage:\n  pi-rust plugins list\n  pi-rust plugins add-root <path> [-l|--project]\n  pi-rust plugins remove-root <path> [-l|--project]\n\nManage plugin discovery roots and inspect plugin runtime diagnostics.\n"
}
