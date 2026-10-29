//! `code-assistant plugin …`: manage plugins from the command line, the
//! same store *Settings → Plugins* edits
//! (`code_assistant_core::plugin_host::PluginStore`).

use crate::cli::PluginCommand;
use anyhow::Result;
use code_assistant_core::plugin_host::{PluginListing, PluginSource, PluginStore};

pub fn run(command: PluginCommand) -> Result<()> {
    let store = PluginStore::new();
    match command {
        PluginCommand::Add { source } => {
            println!("Installing plugin from {source}…");
            let listing = store.install(&source)?;
            println!("Installed:");
            print_listing(&listing);
        }
        PluginCommand::List => {
            let plugins = store.list();
            if plugins.is_empty() {
                println!("No plugins installed ({}).", store.dir().display());
            }
            for listing in &plugins {
                print_listing(listing);
            }
        }
        PluginCommand::Update { name } => {
            let names = match name {
                Some(name) => vec![name],
                None => store
                    .list()
                    .into_iter()
                    .filter(|p| matches!(p.installed.source, PluginSource::Git { .. }))
                    .map(|p| p.name)
                    .collect(),
            };
            for name in names {
                println!("Updating {name}…");
                print_listing(&store.update(&name)?);
            }
        }
        PluginCommand::Remove { name } => {
            store.remove(&name)?;
            println!("Removed plugin '{name}'.");
        }
        PluginCommand::Enable { name } => {
            store.set_enabled(&name, true)?;
            println!("Plugin '{name}' is on.");
        }
        PluginCommand::Disable { name } => {
            store.set_enabled(&name, false)?;
            println!("Plugin '{name}' is off.");
        }
    }
    Ok(())
}

fn print_listing(listing: &PluginListing) {
    let state = if listing.installed.enabled {
        "on"
    } else {
        "off"
    };
    match &listing.manifest {
        Ok(manifest) => {
            let version = manifest.version.as_deref().unwrap_or("-");
            println!("  {} {version} ({state})", listing.name);
            if let Some(description) = &manifest.description {
                println!("    {description}");
            }
            println!("    {}", manifest.contributions_summary());
        }
        Err(error) => println!("  {} ({state}) — not loaded: {error}", listing.name),
    }
    let mut source = listing.installed.source.display();
    if let Some(commit) = &listing.installed.commit {
        source.push_str(&format!(" @ {}", &commit[..commit.len().min(10)]));
    }
    println!("    {source}");
}
