# Model Selection System Implementation Plan

## Overview

This document outlines the implementation plan for replacing the current LLM provider system with a more flexible configuration approach that allows users to configure multiple providers and models through JSON configuration files.

## Goals

1. Replace the existing single `ai-core.json` with a flexible `providers.json` configuration
2. Create a `models.json` configuration that maps display names to provider configurations
3. Add UI controls for model selection in both GPUI and Terminal UI
4. Allow mid-session LLM provider/model changes
5. Clean break from old system - no backward compatibility
6. Keep code compiling throughout implementation phases

## Current System Analysis

### Key Files and Components

**LLM Provider System:**
- `crates/llm/src/factory.rs` - Central factory for creating LLM clients
- `crates/llm/src/config.rs` - AI Core specific configuration
- `crates/llm/src/lib.rs` - Main LLM module with provider trait
- Individual provider implementations: `anthropic.rs`, `openai.rs`, `ollama.rs`, etc.

**Session Management:**
- `crates/code_assistant/src/session/manager.rs` - Manages multiple sessions
- `crates/code_assistant/src/persistence.rs` - Session persistence with `LlmSessionConfig`
- `crates/code_assistant/src/agent/runner.rs` - Agent runner with LLM integration

**Configuration and CLI:**
- `crates/code_assistant/src/cli.rs` - CLI argument parsing for provider selection
- `crates/llm/src/factory.rs` - `LLMClientConfig` and `LLMProviderType` enum

**UI Components:**
- `crates/code_assistant/src/ui/gpui/root.rs` - Main GPUI interface
- `crates/code_assistant/src/ui/terminal/input.rs` - Terminal input handling

### Current Provider Support

The system currently supports these providers (from `LLMProviderType` enum):
- `AiCore` - SAP AI Core with JSON config file
- `Anthropic` - API key via env var
- `Cerebras` - API key via env var
- `Groq` - API key via env var
- `MistralAI` - API key via env var
- `Ollama` - Local installation, no auth
- `OpenAI` - API key via env var
- `OpenAIResponses` - API key via env var (with reasoning support)
- `OpenRouter` - API key via env var
- `Vertex` - Google API key via env var

### Current Configuration Flow

1. CLI args specify provider type, model, base_url, etc.
2. `create_llm_client()` reads env vars or config files per provider
3. `LlmSessionConfig` is persisted with each session
4. Sessions can be loaded with their original LLM configuration

## New Configuration System Design

### providers.json Structure

```json
{
  "anthropic-main": {
    "label": "Anthropic (Main)",
    "provider": "anthropic",
    "config": {
      "api_key": "${ANTHROPIC_API_KEY}",
      "base_url": "https://api.anthropic.com/v1"
    }
  },
  "anthropic-custom": {
    "label": "Anthropic (Custom Endpoint)",
    "provider": "anthropic",
    "config": {
      "api_key": "${ANTHROPIC_CUSTOM_KEY}",
      "base_url": "https://custom.anthropic.endpoint/v1"
    }
  },
  "openai-main": {
    "label": "OpenAI",
    "provider": "openai",
    "config": {
      "api_key": "${OPENAI_API_KEY}",
      "base_url": "https://api.openai.com/v1"
    }
  },
  "ai-core-dev": {
    "label": "SAP AI Core",
    "provider": "ai-core",
    "config": {
      "client_id": "${AI_CORE_CLIENT_ID}",
      "client_secret": "${AI_CORE_CLIENT_SECRET}",
      "token_url": "https://dev.ai-core.com/oauth/token",
      "api_base_url": "https://dev.ai-core.com/v2/inference",
      "models": {
        "claude-sonnet-4": "deployment-uuid-1",
        "gpt-4": "deployment-uuid-2"
      }
    }
  },
  "ollama-local": {
    "label": "Ollama",
    "provider": "ollama",
    "config": {
      "base_url": "http://localhost:11434"
    }
  }
}
```

### models.json Structure

```json
{
  "Claude Sonnet 4.5": {
    "provider": "anthropic-main",
    "id": "claude-sonnet-4-5",
    "config": {
      "thinking_enabled": true,
      "max_tokens": 8192
    }
  },
  "Claude Sonnet 4.5 (Custom)": {
    "provider": "anthropic-custom",
    "id": "claude-sonnet-4-5",
    "config": {
      "thinking_enabled": true,
      "max_tokens": 4096
    }
  },
  "GPT-5 High": {
    "provider": "openai-main",
    "id": "gpt-5",
    "config": {
      "thinking_budget": 10000,
      "temperature": 0.7
    }
  },
  "Llama 3.3 70B": {
    "provider": "ollama-local",
    "id": "llama3.3:70b",
    "config": {
      "num_ctx": 32768,
      "temperature": 0.8
    }
  },
  "AI Core Claude": {
    "provider": "ai-core-dev",
    "id": "claude-sonnet-4",
    "config": {}
  }
}
```

## Implementation Phases

### Phase 1: Configuration System Foundation

**1.1 Create New Configuration Types**
- Create `crates/llm/src/provider_config.rs`:
  - `ProviderConfig` struct with `label`, `provider`, `config` fields
  - `ProvidersConfig` type (HashMap of provider ID to ProviderConfig)
  - `ModelConfig` struct with `provider`, `id`, `config` fields
  - `ModelsConfig` type (HashMap of model display name to ModelConfig)
  - Loading functions for both config files with env var substitution

**1.2 Create Example Configuration Files**
- Create `providers.example.json` in project root with all supported providers
- Create `models.example.json` in project root with example model configurations
- Update README.md with new configuration instructions

### Phase 2: Core Integration

**2.1 Update Session Persistence**
- Modify `crates/code_assistant/src/persistence.rs`:
  - Replace `LlmSessionConfig` with new `SessionModelConfig` containing only `model_name`
  - Remove all old provider-specific fields (provider, base_url, aicore_config, etc.)
  - Update session creation/loading to use model-based config

**2.2 Update Session Manager**
- Modify `crates/code_assistant/src/session/manager.rs`:
  - Add model selection methods
  - Add `change_session_model()` method for mid-session changes
  - Update agent creation to use new config system

**2.3 Update Agent Runner**
- Modify `crates/code_assistant/src/agent/runner.rs`:
  - Update LLM client creation to use model configs
  - Add model change handling
  - Preserve existing model hint functionality

### Phase 3: CLI Integration

**3.1 Replace CLI Arguments**
- Modify `crates/code_assistant/src/cli.rs`:
  - Replace all old provider-specific arguments with single `--model` argument
  - Remove: `--provider`, `--base-url`, `--aicore-config`, `--num-ctx` etc.
  - Add `--list-models` and `--list-providers` commands
  - Update help text and examples for new system only

**3.2 Update Application Initialization**
- Modify application startup to:
  - Load provider and model configurations
  - Resolve model selection from CLI args
  - Require valid model configuration or show helpful error

### Phase 4: GPUI Interface

**4.1 Add Model Selection Dropdown**
- Create `crates/code_assistant/src/ui/gpui/model_selector.rs`:
  - Model selection dropdown component
  - Integration with session manager
  - Update session config for model switching

**4.2 Update Input Area**
- Modify `crates/code_assistant/src/ui/gpui/input_area.rs`:
  - Add model selector underneath input area
  - Handle model selection events
  - Handle model change notifications

### Phase 5: Terminal UI Integration

**5.1 Add Slash Command Support**
- Create `crates/code_assistant/src/ui/terminal/commands.rs`:
  - `/model` command for listing available models
  - `/model <name>` command for switching models
  - `/provider` command for listing providers

**5.2 Update Terminal Input Handler**
- Modify `crates/code_assistant/src/ui/terminal/input.rs`:
  - Detect slash commands
  - Route to command handler
  - Show command help and completion

**5.3 Update Terminal State**
- Modify `crates/code_assistant/src/ui/terminal/state.rs`:
  - Track current model selection
  - Handle model change events
  - Update display to show current model

### Phase 6: Testing and Documentation

**6.1 Update Tests**
- Update `crates/code_assistant/src/tests/mocks.rs`:
  - Add mock provider and model configs
  - Update MockLLMProvider for new system
- Update integration tests for new configuration system
- Add tests for model switching functionality

**6.2 Update Documentation**
- Update README.md with new configuration system only
- Remove all references to old CLI arguments and env var patterns
- Document slash commands and UI controls
- Add troubleshooting section for new config system

### Phase 7: Clean Up Legacy Code

**7.1 Clean Up Factory Code**
- Keep `create_llm_client()` function but ensure it only gets called with config-derived parameters
- Keep `LLMClientConfig` struct - it's still the right abstraction for client creation
- Keep `LLMProviderType` enum - it maps to the "provider" field in providers.json
- Remove any CLI-specific code paths that bypass the config system

**7.2 Remove Old Configuration System**
- Delete `crates/llm/src/config.rs` entirely
- Remove AI Core specific configuration types
- Clean up imports in `crates/llm/src/lib.rs`

**7.3 Final Code Cleanup**
- Remove any remaining references to old CLI arguments
- Clean up unused imports across all files
- Remove temporary stub functions added during implementation
- Run `cargo clippy` and fix any warnings about dead code

## File Changes Summary

### New Files
- `crates/llm/src/provider_config.rs` - New configuration system
- `crates/code_assistant/src/ui/gpui/model_selector.rs` - GPUI model selector
- `crates/code_assistant/src/ui/terminal/commands.rs` - Terminal slash commands
- `providers.example.json` - Example provider configuration
- `models.example.json` - Example model configuration

### Modified Files
- `crates/llm/src/factory.rs` - Completely rewritten factory with new config system
- `crates/llm/src/lib.rs` - Export new config types, remove old exports
- `crates/code_assistant/src/persistence.rs` - New session persistence with model-only config
- `crates/code_assistant/src/session/manager.rs` - Model selection support
- `crates/code_assistant/src/agent/runner.rs` - Updated LLM integration
- `crates/code_assistant/src/cli.rs` - Completely new CLI arguments (breaking change)
- `crates/code_assistant/src/ui/gpui/root.rs` - Model selector integration
- `crates/code_assistant/src/ui/gpui/input_area.rs` - Model display
- `crates/code_assistant/src/ui/terminal/input.rs` - Slash command support
- `crates/code_assistant/src/ui/terminal/state.rs` - Model state tracking
- `crates/code_assistant/src/tests/mocks.rs` - Updated test mocks
- `README.md` - Completely rewritten configuration documentation

### Removed Files
- `crates/llm/src/config.rs` - AI Core specific config (deleted in Phase 7)

## Migration Strategy - Clean Break Approach

### Compilation Continuity During Implementation

1. **Phase-by-Phase Compilation**: Each phase must leave the code in a compilable state
2. **Temporary Stubs**: Keep old functions as stubs that panic with "not implemented" until replaced
3. **Progressive Replacement**: Replace old system piece by piece, removing dead code immediately
4. **No Dual Systems**: Don't maintain both old and new systems simultaneously

### User Migration Path

1. **One-Time Configuration Setup**: Users must manually create `providers.json` and `models.json`
2. **No Automatic Migration**: No code to detect or migrate old `ai-core.json` files
3. **Clear Error Messages**: When old CLI args are used, show clear error with new usage examples
4. **Documentation Focus**: Comprehensive documentation for setting up new configuration

### Implementation Approach

**Phase 1**: Build new config system alongside old (both compile)
**Phase 2**: Replace session persistence, remove old CLI args (breaking change)
**Phase 3**: Replace factory system, remove old LLMClientConfig (breaking change)
**Phase 4-5**: Add UI components using new system only
**Phase 6**: Remove all old code, clean up dead imports

### Breaking Changes Timeline

- **After Phase 2**: Old CLI arguments no longer work
- **After Phase 3**: Old `LLMClientConfig` and env var fallbacks removed
- **After Phase 3**: `crates/llm/src/config.rs` deleted entirely
- **After Phase 6**: All legacy code removed

## Risk Mitigation

1. **Compilation Checks**: Each phase must pass `cargo check` and `cargo test`
2. **Comprehensive Testing**: Test new configuration system thoroughly before removing old code
3. **Clear Error Messages**: When new config is missing, provide helpful setup instructions
4. **Documentation First**: Update README.md early with new configuration requirements
5. **Example Configurations**: Provide complete working examples for all providers

## User Communication

Users will need to:
1. Copy their AI Core config from `~/.config/code-assistant/ai-core.json` to new `providers.json` format
2. Set up `models.json` with their preferred model configurations
3. Update any scripts/aliases to use `--model` instead of `--provider` arguments
4. Set API keys directly in config files or continue using environment variables

This approach results in cleaner, more maintainable code without the complexity of supporting legacy systems.
