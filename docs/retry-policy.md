# Retrying failed requests

A failed LLM request is sent again under one policy, `llm::RetryPolicy`,
stored in `<config_dir>/retry.json` and edited under *Settings → General →
Failed requests*:

```json
{
  "max_retries": 5,
  "max_delay_secs": 300
}
```

- `max_retries` bounds the retries of one request; after the last one the
  session fails and shows the error.
- `max_delay_secs` bounds the wait before a retry, also when the provider asks
  for a longer one (`retry-after`, rate limit reset headers). Below one second
  it counts as one, so retries never follow each other back to back.

Missing fields take the defaults shown above. A changed policy applies to the
next request a session starts: clients and the agent's hooks are created per
run.

## Two places retry

- **The LLM clients** (`llm::utils::handle_retryable_error`) retry what fails
  before an answer streams: rate limits (wait taken from the provider's
  headers, else a minute, doubled per retry), transient authentication
  errors, overloaded, unreachable or failing servers (one second, doubled per
  retry). The factory hands each client the policy
  (`LLMProvider::set_retry_policy`); Ollama and the WebSocket client keep
  their own behaviour. When a client gives up on an error it could have
  retried, it marks it `llm::retry::RetriesExhausted`.
- **The agent loop** (`code_assistant_core::plugins::DefaultRecovery`) retries
  what the clients do not: a broken or stalled stream and an empty answer
  (`agent_core::types::EmptyResponse`). It starts at two seconds, doubling.
  An error marked `RetriesExhausted` is not retried again here, so the retries
  of the two places never multiply.

While waiting, the session shows `RateLimited` (countdown) or `Retrying`
(reason and `attempt/max`).

## Empty answers

An answer without any content used to end the turn as if the model had
answered: nothing was recorded, so the history still ended in the user's
message or the tool results, and the session showed *Resume* without an
error. Now the agent loop treats it as a failed request, retries it under the
policy, and fails visibly when the retries run out.
