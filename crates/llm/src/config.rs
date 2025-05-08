use anyhow::{Context, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct DeploymentConfig {
    pub client_id: String,
    pub client_secret: String,
    pub token_url: String,
    pub api_base_url: String,
}

impl DeploymentConfig {
    const SERVICE_NAME: &'static str = "code-assistant-invoke";
    const USERNAME: &'static str = "default";

    pub fn load() -> Result<Self> {
        let keyring = Entry::new(Self::SERVICE_NAME, Self::USERNAME)?;

        match keyring.get_password() {
            Ok(config_json) => {
                serde_json::from_str(&config_json).with_context(|| "Failed to parse config")
            }
            Err(keyring::Error::NoEntry) => {
                // If no config exists, create it interactively
                let config = Self::create_interactive()?;
                config.save()?;
                Ok(config)
            }
            Err(e) => Err(e).with_context(|| "Failed to access keyring"),
        }
    }

    pub fn save(&self) -> Result<()> {
        let keyring = Entry::new(Self::SERVICE_NAME, Self::USERNAME)?;
        let config_json =
            serde_json::to_string(self).with_context(|| "Failed to serialize config")?;
        keyring
            .set_password(&config_json)
            .with_context(|| "Failed to save config to keyring")
    }

    fn create_interactive() -> Result<Self> {
        use std::io::{self, Write};

        println!("No configuration found. Please enter the following details (they will be stored securely in your keyring):");

        let mut input = String::new();
        let mut config = DeploymentConfig {
            client_id: String::new(),
            client_secret: String::new(),
            token_url: String::new(),
            api_base_url: String::new(),
        };

        print!("Client ID: ");
        io::stdout().flush().unwrap();
        io::stdin().read_line(&mut input).unwrap();
        // Remove whitespace but preserve special characters
        config.client_id = input
            .trim_end_matches(|c| c == '\n' || c == '\r')
            .to_string();
        input.clear();

        print!("Client Secret: ");
        io::stdout().flush().unwrap();
        io::stdin().read_line(&mut input).unwrap();
        // Same for secret
        config.client_secret = input
            .trim_end_matches(|c| c == '\n' || c == '\r')
            .to_string();
        input.clear();

        print!("Token URL: ");
        io::stdout().flush().unwrap();
        io::stdin().read_line(&mut input).unwrap();
        config.token_url = input.trim().to_string();
        input.clear();

        print!("API Base URL: ");
        io::stdout().flush().unwrap();
        io::stdin().read_line(&mut input).unwrap();
        config.api_base_url = input.trim().to_string();
        input.clear();

        Ok(config)
    }
}
