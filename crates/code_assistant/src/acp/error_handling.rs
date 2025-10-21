use agent_client_protocol as acp;
use serde_json::json;

/// Format configuration errors with helpful context
pub fn format_config_error(error: &anyhow::Error) -> String {
    let error_str = error.to_string();

    // Check for common configuration issues
    if error_str.contains("configuration file not found") {
        format!(
            "{error_str}\n\n**What to do:**\n\
            1. Copy the example configuration files to the expected location\n\
            2. Update the configuration with your API keys and settings\n\
            3. Restart the agent\n\n\
            **Need help?** Check the documentation for configuration setup.",
        )
    } else if error_str.contains("environment variable") {
        format!(
            "{error_str}\n\n**What to do:**\n\
            1. Set the missing environment variable\n\
            2. Or update your configuration to use a different authentication method\n\
            3. Restart the agent",
        )
    } else if error_str.contains("API key") || error_str.contains("authentication") {
        format!(
            "{error_str}\n\n**What to do:**\n\
            1. Check that your API key is correct in the configuration\n\
            2. Verify the API key has the necessary permissions\n\
            3. Check if the API key has expired",
        )
    } else {
        format!(
            "{error_str}\n\n**What to do:**\n\
            1. Check your configuration files for syntax errors\n\
            2. Verify all required fields are present\n\
            3. Check the logs for more detailed error information",
        )
    }
}

/// Convert common errors to appropriate ACP errors with detailed messages
pub fn to_acp_error(error: &anyhow::Error) -> acp::Error {
    let error_str = error.to_string();
    let error_lower = error_str.to_ascii_lowercase();

    if error_lower.contains("configuration file not found")
        || error_lower.contains("environment variable")
        || error_lower.contains("api key")
    {
        // Configuration errors - these are client-side issues
        let formatted = format_config_error(error);
        acp::Error::new((acp::ErrorCode::INVALID_PARAMS.code, error_str)).with_data(formatted)
    } else if error_lower.contains("401") || error_lower.contains("unauthorized") {
        // Authentication errors
        acp::Error::new((acp::ErrorCode::AUTH_REQUIRED.code, error_str.clone()))
            .with_data(error_str)
    } else if error_lower.contains("404") || error_lower.contains("not found") {
        // Resource not found errors
        acp::Error::new((acp::ErrorCode::RESOURCE_NOT_FOUND.code, error_str.clone()))
            .with_data(error_str)
    } else if error_lower.contains("400")
        || error_lower.contains("bad request")
        || error_lower.contains("invalid request")
    {
        // Client provided invalid parameters
        acp::Error::new((acp::ErrorCode::INVALID_PARAMS.code, error_str.clone()))
            .with_data(json!({ "hint": "The model configuration may include custom fields under `config` that this provider does not accept."}))
    } else {
        // All other errors are internal
        acp::Error::new((acp::ErrorCode::INTERNAL_ERROR.code, error_str.clone()))
            .with_data(error_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn test_format_config_error_missing_file() {
        let error = anyhow!("Providers configuration file not found.\nSearched locations:\n  /home/user/.config/code-assistant/providers.json\n\nPlease copy providers.example.json to /home/user/.config/code-assistant/providers.json and configure it.");

        let formatted = format_config_error(&error);

        assert!(formatted.contains("configuration file not found"));
        assert!(formatted.contains("**What to do:**"));
        assert!(formatted.contains("Copy the example configuration files"));
        assert!(formatted.contains("Update the configuration with your API keys"));
    }

    #[test]
    fn test_format_config_error_env_var() {
        let error = anyhow!("Environment variable OPENAI_API_KEY not found");

        let formatted = format_config_error(&error);

        assert!(formatted.contains("Environment variable"));
        assert!(formatted.contains("**What to do:**"));
        assert!(
            formatted.contains("Set the missing environment variable")
                || formatted.contains("Check your configuration files")
        );
    }

    #[test]
    fn test_to_acp_error_config() {
        let error = anyhow!("Providers configuration file not found");
        let acp_error = to_acp_error(&error);

        // Should be invalid_params for configuration errors
        assert_eq!(acp_error.code, acp::ErrorCode::INVALID_PARAMS.code);
        assert!(acp_error
            .message
            .contains("Providers configuration file not found"));
        assert!(acp_error.data.is_some());
    }

    #[test]
    fn test_to_acp_error_auth() {
        let error = anyhow!("HTTP 401: Unauthorized");
        let acp_error = to_acp_error(&error);

        // Should be auth_required for 401 errors - but using -32000 range
        assert_eq!(acp_error.code, acp::ErrorCode::AUTH_REQUIRED.code);
        assert!(acp_error.message.contains("HTTP 401"));
    }

    #[test]
    fn test_to_acp_error_not_found() {
        let error = anyhow!("HTTP 404: Not found");
        let acp_error = to_acp_error(&error);

        // Should be resource_not_found for 404 errors
        assert_eq!(acp_error.code, acp::ErrorCode::RESOURCE_NOT_FOUND.code);
        assert!(acp_error.message.contains("HTTP 404"));
    }

    #[test]
    fn test_to_acp_error_internal() {
        let error = anyhow!("Some unexpected error");
        let acp_error = to_acp_error(&error);

        // Should be internal_error for other errors
        assert_eq!(acp_error.code, acp::ErrorCode::INTERNAL_ERROR.code);
        assert!(acp_error.message.contains("Some unexpected error"));
    }

    #[test]
    fn test_to_acp_error_bad_request() {
        let error = anyhow!("HTTP 400: Bad request due to invalid payload");
        let acp_error = to_acp_error(&error);

        assert_eq!(acp_error.code, acp::ErrorCode::INVALID_PARAMS.code);
        assert!(acp_error
            .message
            .contains("HTTP 400: Bad request due to invalid payload"));
        assert!(acp_error.data.is_none());
    }

    #[test]
    fn test_to_acp_error_bad_request_with_hint() {
        let error = anyhow!(
            "Invalid request: {{\"message\":\"thinking_budget: Extra inputs are not permitted\"}}"
        );
        let acp_error = to_acp_error(&error);

        assert_eq!(acp_error.code, acp::ErrorCode::INVALID_PARAMS.code);
        assert!(acp_error
            .message
            .contains("thinking_budget: Extra inputs are not permitted"));
        let data = acp_error.data.expect("expected hint data");
        let hint = data
            .get("hint")
            .and_then(|value| value.as_str())
            .expect("hint should be a string");
        assert!(hint.contains("custom fields"));
    }
}
