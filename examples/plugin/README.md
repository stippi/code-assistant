# Example plugin

A code-assistant plugin showing the three kinds of contributions. Copy the
folder into a git repository of its own to start a plugin, or install it
from here to try it:

```bash
code-assistant plugin add examples/plugin
```

(or *Settings → Plugins*, then paste the folder or a git URL and click
*Install*.) A local folder is linked, so edits to `plugin.json` show at once.

What it adds:

- **Provider templates** — *Settings → Providers → Add* offers "Ollama (this
  machine)" and "LiteLLM proxy": picking one fills in the label, the
  provider type and the base URL; you add the API key.
- **An MCP server** — `fetch` (`uvx mcp-server-fetch`), listed under *From
  plugins* in *Settings → MCP Servers*, read-only, and in every session's
  *Tools* menu. Needs [uv](https://docs.astral.sh/uv/).
- **A file viewer** — `.drawio` and `.drawio.svg` files open in draw.io in
  the Files view; edits are saved to the file. `scripts/start-drawio.sh`
  starts draw.io in Docker when nothing answers at `localhost:8080`. A
  draw.io you run yourself on that port is used as is.

The manifest format is described in `docs/plugins.md`.
