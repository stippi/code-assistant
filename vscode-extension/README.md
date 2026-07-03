# Code Assistant for VS Code

Chat with [code-assistant](https://github.com/stippi/code-assistant) directly in VS Code.
The extension connects to the `code-assistant` CLI via the
[Agent Client Protocol](https://agentclientprotocol.com) (`code-assistant acp`).

## Requirements

The `code-assistant` CLI must be installed and on your `PATH`
(or configure `codeAssistant.commandPath`):

```sh
brew install stippi/code-assistant/code-assistant
```

## Settings

- `codeAssistant.commandPath` — path to the executable (default: `code-assistant`)
- `codeAssistant.extraArgs` — extra arguments for `code-assistant acp`,
  e.g. `["--model", "sonnet-4.5"]`

## Development

```sh
npm install
npm run build      # bundle extension + webview into dist/
npm run watch      # rebuild on change
npm run typecheck  # typecheck extension and webview sources
```

Launch with the "Run Extension" target in VS Code, or `code --extensionDevelopmentPath=$(pwd)`.
