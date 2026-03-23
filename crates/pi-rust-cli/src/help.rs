use super::PackageCommand;

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
