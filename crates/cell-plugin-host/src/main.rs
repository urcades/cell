use std::env;
use std::process::ExitCode;

use cell_plugin_host::{PluginHost, PluginHostConfig, discover_plugins};

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    match args.as_slice() {
        [] => {
            eprintln!("usage: cell-plugin-host discover <roots...> | launch <descriptor>");
            Ok(())
        }
        [command, rest @ ..] if command == "discover" => {
            let roots = if rest.is_empty() {
                vec![std::env::current_dir()?]
            } else {
                rest.iter().map(std::path::PathBuf::from).collect()
            };
            let plugins = discover_plugins(&roots)?;
            println!("{}", serde_json::to_string_pretty(&plugins)?);
            Ok(())
        }
        [command, descriptor] if command == "launch" => {
            let host = PluginHost::new(PluginHostConfig {
                discovery_roots: vec![std::path::PathBuf::from(".")],
                ..PluginHostConfig::default()
            });
            let discovered = cell_plugin_host::DiscoveredPlugin::load(descriptor)?;
            let registered = host.launch_and_register(discovered)?;
            println!("{}", serde_json::to_string_pretty(&registered.summary())?);
            Ok(())
        }
        _ => {
            eprintln!("usage: cell-plugin-host discover <roots...> | launch <descriptor>");
            Ok(())
        }
    }
}
