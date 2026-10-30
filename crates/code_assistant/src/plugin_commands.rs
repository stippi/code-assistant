//! `code-assistant plugin …`: manage plugins and marketplaces from the
//! command line, the same store *Settings → Plugins* edits
//! (`code_assistant_core::plugin_host::PluginStore`).

use crate::cli::{MarketplaceCommand, PluginCommand};
use anyhow::Result;
use code_assistant_core::plugin_host::{
    Added, MarketplaceListing, PluginListing, PluginSource, PluginStore,
};
use std::collections::BTreeSet;
use std::io::IsTerminal;

pub fn run(command: PluginCommand) -> Result<()> {
    // On a terminal git may ask for a login its credential helpers lack.
    let store = PluginStore::new().interactive(std::io::stdin().is_terminal());
    match command {
        PluginCommand::Add { source } => {
            println!("Adding {source}…");
            match store.add(&source)? {
                Added::Plugin(listing) => {
                    println!("Installed:");
                    print_listing(&listing);
                }
                Added::Marketplace(listing) => {
                    println!("Added marketplace:");
                    print_marketplace(&listing, &installed_names(&store));
                    println!(
                        "Install its plugins with `code-assistant plugin add <plugin>@{}`.",
                        listing.name
                    );
                }
            }
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
        PluginCommand::Update { name: Some(name) } => {
            println!("Updating {name}…");
            print_listing(&store.update(&name)?);
        }
        PluginCommand::Update { name: None } => {
            for marketplace in store.list_marketplaces() {
                println!("Updating marketplace {}…", marketplace.name);
                print_marketplace(
                    &store.update_marketplace(&marketplace.name)?,
                    &installed_names(&store),
                );
            }
            let plugins = store
                .list()
                .into_iter()
                .filter(|p| matches!(p.installed.source, PluginSource::Git { .. }));
            for plugin in plugins {
                println!("Updating {}…", plugin.name);
                print_listing(&store.update(&plugin.name)?);
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
        PluginCommand::Marketplace(command) => run_marketplace(&store, command)?,
    }
    Ok(())
}

fn run_marketplace(store: &PluginStore, command: MarketplaceCommand) -> Result<()> {
    match command {
        MarketplaceCommand::Add { source } => {
            println!("Adding marketplace {source}…");
            let listing = store.add_marketplace(&source)?;
            print_marketplace(&listing, &installed_names(store));
        }
        MarketplaceCommand::List => {
            let marketplaces = store.list_marketplaces();
            if marketplaces.is_empty() {
                println!("No marketplaces added.");
            }
            let installed = installed_names(store);
            for listing in &marketplaces {
                print_marketplace(listing, &installed);
            }
        }
        MarketplaceCommand::Update { name } => {
            let names = match name {
                Some(name) => vec![name],
                None => store
                    .list_marketplaces()
                    .into_iter()
                    .map(|m| m.name)
                    .collect(),
            };
            for name in names {
                println!("Updating marketplace {name}…");
                print_marketplace(&store.update_marketplace(&name)?, &installed_names(store));
            }
        }
        MarketplaceCommand::Remove { name } => {
            store.remove_marketplace(&name)?;
            println!("Removed marketplace '{name}' and the plugins installed from it.");
        }
    }
    Ok(())
}

fn installed_names(store: &PluginStore) -> BTreeSet<String> {
    store.record().plugins.into_keys().collect()
}

fn print_marketplace(listing: &MarketplaceListing, installed: &BTreeSet<String>) {
    let mut source = listing.installed.source.display();
    if let Some(commit) = &listing.installed.commit {
        source.push_str(&format!(" @ {}", &commit[..commit.len().min(10)]));
    }
    println!("  {} — {source}", listing.name);
    match &listing.manifest {
        Ok(manifest) => {
            if let Some(description) = &manifest.description {
                println!("    {description}");
            }
            for entry in &manifest.plugins {
                let state = if installed.contains(&entry.name) {
                    " (installed)"
                } else {
                    ""
                };
                let description = entry
                    .description
                    .as_deref()
                    .map(|d| format!(" — {d}"))
                    .unwrap_or_default();
                println!("    {}@{}{state}{description}", entry.name, listing.name);
            }
        }
        Err(error) => println!("    not loaded: {error}"),
    }
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
