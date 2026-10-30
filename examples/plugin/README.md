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
- **File renderers** — in the Files view, `.dot` and `.gv` files show as
  graphs (`dot -Tsvg`, needs [Graphviz](https://graphviz.org)), and
  `.drawio` files open in draw.io as a web editor (macOS and Windows); edits
  are saved to the file. Its `start` command runs draw.io in Docker
  (`jgraph/drawio`) when nothing answers at `localhost:8080`; a command,
  not a shell script, so it works on Windows too. A draw.io you run
  yourself on that port is used as is.

The manifest format is described in `docs/plugins.md`.
