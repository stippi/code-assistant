# Parallel Search MCP

[Parallel Search MCP](https://docs.parallel.ai/integrations/mcp/search-mcp)
offers web search and page fetching without a Parallel API key. The anonymous
endpoint is free for light use, with lower rate limits than authenticated access.
This optional configuration uses code-assistant's existing Streamable HTTP MCP
client and leaves your model and other MCP servers unchanged.

## Enable it

Merge the `parallel` entry from
[`parallel-search.json`](../crates/mcp_client/examples/parallel-search.json)
into the `servers` object in `~/.config/code-assistant/mcp-servers.json`:

```json
{
  "servers": {
    "parallel": {
      "url": "https://search.parallel.ai/mcp",
      "headers": {
        "User-Agent": "code-assistant/parallel-search-example"
      },
      "enabled_tools": ["web_search", "web_fetch"]
    }
  }
}
```

Keep any existing server entries. No token, OAuth login, or environment variable
is needed for this endpoint. Configuration changes apply on the next agent run.
The registered tools are `mcp__parallel__web_search` (search objectives and
queries) and `mcp__parallel__web_fetch` (read specific URLs). You can remove the
entry or set `enabled` to `false` to disable it.

## Run the registry example

With the [Rust toolchain](https://rustup.rs/) installed, clone this repository
and run from its root:

```sh
cargo run -p mcp_client --example parallel_search
```

Cargo installs the dependencies declared by `mcp_client`. The example loads the
same JSON configuration, discovers the two tools through `register_mcp_tools`,
and invokes them through `ToolRegistry`. It prints search excerpts about Rust
ownership and fetches the Rust book's ownership page. It does not load saved
OAuth credentials or call an LLM. Each registration or tool call is bounded by
a 60-second timeout; connection errors and server-reported tool errors produce
a nonzero exit status.
