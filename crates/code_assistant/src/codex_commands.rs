//! CLI commands for Codex (ChatGPT subscription) authentication.
//!
//! These commands manage the OAuth login flow that lets users authenticate
//! with their existing ChatGPT Plus/Pro/Team subscription instead of
//! needing a separate OpenAI API key.
//!
//! Auth tokens are stored inside `providers.json` under the provider's
//! `config.codex_tokens` field.

use anyhow::Result;
use llm::codex_auth::{self, CodexTokenStorage, ProvidersJsonTokenStorage};
use std::sync::Arc;

/// The default provider ID used for ChatGPT subscription auth.
const PROVIDER_ID: &str = codex_auth::DEFAULT_PROVIDER_ID;

/// Build the default providers.json storage backend.
fn default_storage() -> Arc<dyn CodexTokenStorage> {
    Arc::new(ProvidersJsonTokenStorage::new(
        PROVIDER_ID.to_string(),
        None,
    ))
}

/// Run the Codex OAuth browser login flow.
pub async fn run_codex_login() -> Result<()> {
    let storage = default_storage();

    // Check if already authenticated
    let status = codex_auth::get_auth_status(storage.as_ref());
    if status.authenticated {
        println!(
            "Already logged in as {} (plan: {}).",
            status.email.as_deref().unwrap_or("unknown"),
            status.plan_type.as_deref().unwrap_or("unknown"),
        );
        println!("Run `codex-logout` first to log in with a different account.");
        return Ok(());
    }

    println!("Starting ChatGPT subscription login...");
    println!();

    let (authorize_url, rx) = codex_auth::start_login_flow(storage.clone()).await?;

    println!("Opening your browser to authenticate.");
    println!("If the browser doesn't open, visit this URL manually:");
    println!();
    println!("  {}", authorize_url);
    println!();
    println!("Waiting for authentication (up to 5 minutes)...");

    // Try to open the browser
    if let Err(e) = open::that(&authorize_url) {
        eprintln!("Could not open browser: {e}");
    }

    // Wait for the callback
    let result = rx.await??;

    let (email, plan) = {
        let status = codex_auth::get_auth_status(storage.as_ref());
        (status.email, status.plan_type)
    };

    println!();
    println!("Login successful!");
    if let Some(email) = &email {
        println!("  Email: {}", email);
    }
    if let Some(plan) = &plan {
        println!("  Plan:  {}", plan);
    }
    println!();
    println!("Tokens stored in providers.json under \"{}\".", PROVIDER_ID);
    println!();
    println!("Make sure your providers.json contains an entry like:");
    println!(r#"  "{}": {{"#, PROVIDER_ID);
    println!(r#"    "label": "ChatGPT Subscription (WebSocket)","#);
    println!(r#"    "provider": "openai-responses-ws","#);
    println!(r#"    "config": {{ "codex_auth": true }}"#);
    println!(r#"  }}"#);
    println!();
    println!("(The login flow creates this automatically if it doesn't exist.)");

    let _ = result; // LoginResult consumed

    Ok(())
}

/// Remove stored Codex auth tokens.
pub fn run_codex_logout() -> Result<()> {
    let storage = default_storage();

    // Check if tokens exist first
    let status = codex_auth::get_auth_status(storage.as_ref());
    if !status.authenticated {
        println!("Not logged in (no tokens found).");
        return Ok(());
    }

    storage.delete()?;
    println!(
        "Logged out. Tokens removed from providers.json (provider: \"{}\").",
        PROVIDER_ID
    );
    Ok(())
}

/// Show current Codex auth status.
pub fn run_codex_status() -> Result<()> {
    let storage = default_storage();
    let status = codex_auth::get_auth_status(storage.as_ref());

    if status.authenticated {
        println!("Authenticated: yes");
        if let Some(email) = &status.email {
            println!("Email:         {}", email);
        }
        if let Some(plan) = &status.plan_type {
            println!("Plan:          {}", plan);
        }
        println!(
            "Needs refresh: {}",
            if status.needs_refresh { "yes" } else { "no" }
        );
        println!("Provider:      \"{}\" in providers.json", PROVIDER_ID);
    } else {
        println!("Not authenticated.");
        println!("Run `code-assistant codex-login` to log in with your ChatGPT subscription.");
    }

    Ok(())
}
