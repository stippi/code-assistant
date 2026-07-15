use anyhow::Result;
use llm::provider_config::ConfigurationSystem;

/// Static descriptor for a slash command, used for autocomplete and help display.
pub struct SlashCommand {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
}

/// All registered slash commands.
///
/// This slice is the single source of truth for command discovery. Every entry here
/// corresponds to a match arm in `CommandProcessor::process_command`. Keeping them
/// in sync is intentional: adding a command requires updating both places.
pub fn all_commands() -> &'static [SlashCommand] {
    &[
        SlashCommand {
            name: "help",
            aliases: &["h"],
            description: "Show available commands",
        },
        SlashCommand {
            name: "model",
            aliases: &["m"],
            description: "List available models or switch to one: /model <name>",
        },
        SlashCommand {
            name: "provider",
            aliases: &["p"],
            description: "List available LLM providers",
        },
        SlashCommand {
            name: "current",
            aliases: &["c"],
            description: "Show the currently active model",
        },
        SlashCommand {
            name: "plan",
            aliases: &[],
            description: "Toggle the plan view panel",
        },
        SlashCommand {
            name: "clear",
            aliases: &[],
            description: "Clear the conversation context",
        },
        SlashCommand {
            name: "compact",
            aliases: &[],
            description: "Summarize and compact the conversation context",
        },
        SlashCommand {
            name: "permissions",
            aliases: &[],
            description:
                "Show or set the permission tier: /permissions [bypass-all|outward-tools|write-tools|all-tools]",
        },
        SlashCommand {
            name: "allow",
            aliases: &[],
            description: "Allow the pending tool permission request once",
        },
        SlashCommand {
            name: "always",
            aliases: &[],
            description: "Allow the pending tool permission request for this session",
        },
        SlashCommand {
            name: "deny",
            aliases: &[],
            description: "Deny the pending tool permission request",
        },
        SlashCommand {
            name: "skill",
            aliases: &[],
            description: "Activate a skill: /skill <name> (or pick from the list)",
        },
        SlashCommand {
            name: "sessions",
            aliases: &["resume"],
            description: "Switch to another session (pick from the list)",
        },
    ]
}

/// Result of processing a slash command
#[derive(Debug, Clone)]
pub enum CommandResult {
    /// Continue normal operation
    Continue,
    /// Display help information
    Help(String),
    /// List available models
    ListModels,
    /// List available providers
    ListProviders,
    /// Switch to a specific model
    SwitchModel(String),
    /// Display current model information
    ShowCurrentModel,
    /// Invalid command
    InvalidCommand(String),
    /// Toggle plan rendering mode
    TogglePlan,

    /// Clear conversation context
    ClearContext,
    /// Compact (summarise) conversation context
    CompactContext,
    /// Open the skill picker popup.
    OpenSkillPicker,
    /// Open the session picker popup.
    OpenSessionPicker,
    /// Switch the terminal to another session, loading its transcript.
    SwitchSession(String),
    /// Activate a skill by name. `scope` is the scope token (project name, or
    /// `:config:` / `:system:`); `None` means resolve it from the cached
    /// catalog by name.
    InvokeSkill { scope: Option<String>, name: String },
    /// Show the current permission tier.
    ShowPermissionTier,
    /// Switch the permission tier.
    SetPermissionTier(tools_core::permissions::PermissionTier),
    /// Answer a tool permission request. `request_id: None` (slash commands)
    /// answers the oldest pending request; the permission prompt popup
    /// carries the id of the request it was opened for.
    RespondPermission {
        request_id: Option<String>,
        decision: tools_core::PermissionDecision,
    },
}

/// Process slash commands in terminal UI
pub struct CommandProcessor {
    config: ConfigurationSystem,
}

impl CommandProcessor {
    pub fn new() -> Result<Self> {
        let config = ConfigurationSystem::load()?;
        Ok(Self { config })
    }

    /// Process a slash command and return the result
    pub fn process_command(&self, input: &str) -> CommandResult {
        let input = input.trim();

        if !input.starts_with('/') {
            return CommandResult::Continue;
        }

        let parts: Vec<&str> = input[1..].split_whitespace().collect();
        if parts.is_empty() {
            return CommandResult::Help(self.get_help_text());
        }

        match parts[0].to_lowercase().as_str() {
            "help" | "h" => CommandResult::Help(self.get_help_text()),
            "model" | "m" => self.process_model_command(&parts[1..]),
            "provider" | "p" => self.process_provider_command(&parts[1..]),
            "current" | "c" => CommandResult::ShowCurrentModel,
            "plan" => CommandResult::TogglePlan,
            "clear" => CommandResult::ClearContext,
            "compact" => CommandResult::CompactContext,
            "permissions" => Self::process_permissions_command(&parts[1..]),
            "allow" => CommandResult::RespondPermission {
                request_id: None,
                decision: tools_core::PermissionDecision::GrantedOnce,
            },
            "always" => CommandResult::RespondPermission {
                request_id: None,
                decision: tools_core::PermissionDecision::GrantedSession,
            },
            "deny" => CommandResult::RespondPermission {
                request_id: None,
                decision: tools_core::PermissionDecision::Denied,
            },
            "skill" => {
                if parts.len() > 1 {
                    CommandResult::InvokeSkill {
                        scope: None,
                        name: parts[1..].join(" "),
                    }
                } else {
                    CommandResult::OpenSkillPicker
                }
            }
            // Both `/sessions` and `/resume` open the session picker; any
            // trailing text is ignored (selection happens in the picker).
            "sessions" | "resume" => CommandResult::OpenSessionPicker,
            _ => CommandResult::InvalidCommand(format!("Unknown command: /{}", parts[0])),
        }
    }

    fn process_model_command(&self, args: &[&str]) -> CommandResult {
        if args.is_empty() {
            return CommandResult::ListModels;
        }

        let model_name = args.join(" ");

        // Check if the model exists
        if self.config.models.contains_key(&model_name) {
            CommandResult::SwitchModel(model_name)
        } else {
            CommandResult::InvalidCommand(format!(
                "Model '{model_name}' not found. Use '/model' to list available models.",
            ))
        }
    }

    fn process_permissions_command(args: &[&str]) -> CommandResult {
        use tools_core::permissions::PermissionTier;
        match args {
            [] => CommandResult::ShowPermissionTier,
            [tier] => match tier.to_lowercase().as_str() {
                "bypass-all" | "bypass" => {
                    CommandResult::SetPermissionTier(PermissionTier::BypassAll)
                }
                "outward-tools" | "outward" => {
                    CommandResult::SetPermissionTier(PermissionTier::OutwardTools)
                }
                "write-tools" | "write" => {
                    CommandResult::SetPermissionTier(PermissionTier::WriteTools)
                }
                "all-tools" | "all" => CommandResult::SetPermissionTier(PermissionTier::AllTools),
                other => CommandResult::InvalidCommand(format!(
                    "Unknown permission tier '{other}'. Use bypass-all, outward-tools, write-tools or all-tools.",
                )),
            },
            _ => CommandResult::InvalidCommand(
                "Usage: /permissions [bypass-all|outward-tools|write-tools|all-tools]".to_string(),
            ),
        }
    }

    fn process_provider_command(&self, args: &[&str]) -> CommandResult {
        if args.is_empty() {
            return CommandResult::ListProviders;
        }

        CommandResult::InvalidCommand(
            "Provider switching not supported. Use '/model <name>' to switch models.".to_string(),
        )
    }

    fn get_help_text(&self) -> String {
        let mut text = String::from("Available commands:\n");
        for cmd in all_commands() {
            if cmd.aliases.is_empty() {
                text.push_str(&format!("  /{:<16} {}\n", cmd.name, cmd.description));
            } else {
                let alias_list = cmd
                    .aliases
                    .iter()
                    .map(|a| format!("/{a}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                text.push_str(&format!(
                    "  /{}, {:<12} {}\n",
                    cmd.name, alias_list, cmd.description
                ));
            }
        }
        text.push_str("\nExamples:\n  /model Claude Sonnet 4.5\n  /model GPT-5");
        text
    }

    /// Get formatted list of available models
    pub fn get_models_list(&self) -> String {
        let mut models: Vec<_> = self.config.models.keys().cloned().collect();
        models.sort();

        let mut output = String::from("Available models:\n");
        for model_name in models {
            if let Ok((_model_config, provider_config)) =
                self.config.get_model_with_provider(&model_name)
            {
                output.push_str(&format!("  {} ({})\n", model_name, provider_config.label));
            }
        }
        output
    }

    /// Get formatted list of available providers
    pub fn get_providers_list(&self) -> String {
        let mut providers: Vec<_> = self.config.providers.keys().cloned().collect();
        providers.sort();

        let mut output = String::from("Available providers:\n");
        for provider_id in providers {
            if let Some(provider) = self.config.providers.get(&provider_id) {
                output.push_str(&format!("  {} ({})\n", provider_id, provider.label));
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a processor without touching the runner's home directory: the
    /// session/routing commands don't read config, and `new()`'s disk load
    /// would fail in CI (no `providers.json`).
    fn test_processor() -> CommandProcessor {
        let config = ConfigurationSystem {
            providers: Default::default(),
            models: Default::default(),
        };
        CommandProcessor { config }
    }

    #[test]
    fn sessions_command_is_registered_with_resume_alias() {
        let cmd = all_commands()
            .iter()
            .find(|c| c.name == "sessions")
            .expect("`sessions` command should be registered");
        assert!(cmd.aliases.contains(&"resume"));
    }

    #[test]
    fn sessions_and_resume_open_the_session_picker() {
        let processor = test_processor();
        assert!(matches!(
            processor.process_command("/sessions"),
            CommandResult::OpenSessionPicker
        ));
        assert!(matches!(
            processor.process_command("/resume"),
            CommandResult::OpenSessionPicker
        ));
        // Trailing text is ignored — the picker handles selection.
        assert!(matches!(
            processor.process_command("/sessions foo bar"),
            CommandResult::OpenSessionPicker
        ));
    }
}
