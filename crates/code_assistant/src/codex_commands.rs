//! CLI commands for Codex (ChatGPT subscription) authentication.
//!
//! These commands manage the OAuth login flow that lets users authenticate
//! with their existing ChatGPT Plus/Pro/Team subscription instead of
//! needing a separate OpenAI API key.

use anyhow::Result;
use llm::codex_auth;

/// Run the Codex OAuth browser login flow.
pub async fn run_codex_login() -> Result<()> {
    let auth_path = codex_auth::default_codex_auth_path();

    // Check if already authenticated
    if let Ok(Some(_)) = codex_auth::load_auth_state(Some(&auth_path)) {
        let status = codex_auth::get_auth_status(Some(&auth_path));
        if status.authenticated {
            println!(
                "Already logged in as {} (plan: {}).",
                status.email.as_deref().unwrap_or("unknown"),
                status.plan_type.as_deref().unwrap_or("unknown"),
            );
            println!("Run `codex-logout` first to log in with a different account.");
            return Ok(());
        }
    }

    println!("Starting ChatGPT subscription login...");
    println!();

    let (authorize_url, rx) = codex_auth::start_login_flow(Some(&auth_path)).await?;

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
        let status = codex_auth::get_auth_status(Some(&auth_path));
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
    println!("Tokens stored at: {}", auth_path.display());
    println!();
    println!("To use this with a model, add to your providers.json:");
    println!(r#"  "openai-chatgpt": {{"#);
    println!(r#"    "label": "ChatGPT Subscription (WebSocket)","#);
    println!(r#"    "provider": "openai-responses-ws","#);
    println!(r#"    "config": {{ "codex_auth": true }}"#);
    println!(r#"  }}"#);

    let _ = result; // LoginResult consumed

    Ok(())
}

/// Remove stored Codex auth tokens.
pub fn run_codex_logout() -> Result<()> {
    let auth_path = codex_auth::default_codex_auth_path();

    if !auth_path.exists() {
        println!("Not logged in (no tokens found).");
        return Ok(());
    }

    codex_auth::delete_auth_state(Some(&auth_path))?;
    println!("Logged out. Tokens removed from {}", auth_path.display());
    Ok(())
}

/// Show current Codex auth status.
pub fn run_codex_status() -> Result<()> {
    let auth_path = codex_auth::default_codex_auth_path();
    let status = codex_auth::get_auth_status(Some(&auth_path));

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
        println!("Token file:    {}", auth_path.display());
    } else {
        println!("Not authenticated.");
        println!("Run `code-assistant codex-login` to log in with your ChatGPT subscription.");
    }

    Ok(())
}
