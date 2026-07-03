import * as vscode from "vscode";
import type * as acp from "@agentclientprotocol/sdk";
import { AgentConnection } from "./acp/connection";

type WebviewInbound =
  | { type: "ready" }
  | { type: "prompt"; text: string }
  | { type: "cancel" }
  | { type: "setConfigOption"; configId: string; value: string };

type WebviewOutbound =
  | { type: "sessionUpdate"; update: acp.SessionUpdate }
  | { type: "turnEnded"; stopReason: acp.StopReason }
  | { type: "configOptions"; options: acp.SessionConfigOption[] }
  | { type: "error"; message: string }
  | { type: "reset" };

export class ChatViewProvider implements vscode.WebviewViewProvider {
  static readonly viewId = "codeAssistant.chatView";

  private view?: vscode.WebviewView;
  private agent?: AgentConnection;
  private agentStarting?: Promise<AgentConnection>;
  private readonly output = vscode.window.createOutputChannel("Code Assistant");

  constructor(private readonly extensionUri: vscode.Uri) {}

  resolveWebviewView(view: vscode.WebviewView): void {
    this.view = view;
    view.webview.options = {
      enableScripts: true,
      localResourceRoots: [vscode.Uri.joinPath(this.extensionUri, "dist")],
    };
    view.webview.html = this.renderHtml(view.webview);
    view.webview.onDidReceiveMessage((msg: WebviewInbound) => {
      void this.onMessage(msg);
    });
  }

  newSession(): void {
    this.agent?.dispose();
    this.agent = undefined;
    this.agentStarting = undefined;
    this.post({ type: "reset" });
    this.connectAgent();
  }

  /** Start the agent in the background so config options (model selector)
   *  are available before the first prompt. */
  private connectAgent(): void {
    void this.ensureAgent()
      .then((agent) => {
        this.post({ type: "configOptions", options: agent.configOptions });
      })
      .catch((err: unknown) => {
        const message = err instanceof Error ? err.message : String(err);
        this.post({
          type: "error",
          message: `${message} — is code-assistant installed? Set "codeAssistant.commandPath" if it is not on your PATH.`,
        });
      });
  }

  dispose(): void {
    this.agent?.dispose();
    this.output.dispose();
  }

  private async onMessage(msg: WebviewInbound): Promise<void> {
    switch (msg.type) {
      case "ready":
        this.connectAgent();
        break;
      case "prompt":
        await this.handlePrompt(msg.text);
        break;
      case "cancel":
        await this.agent?.cancel();
        break;
      case "setConfigOption": {
        const agent = this.agent;
        if (!agent) {
          break;
        }
        try {
          const options = await agent.setConfigOption(msg.configId, msg.value);
          this.post({ type: "configOptions", options });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          this.post({ type: "error", message });
        }
        break;
      }
    }
  }

  private async handlePrompt(text: string): Promise<void> {
    try {
      const agent = await this.ensureAgent();
      agent.prompt(text);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.post({
        type: "error",
        message: `${message} — is code-assistant installed? Set "codeAssistant.commandPath" if it is not on your PATH.`,
      });
    }
  }

  private ensureAgent(): Promise<AgentConnection> {
    if (this.agent) {
      return Promise.resolve(this.agent);
    }
    if (!this.agentStarting) {
      this.agentStarting = this.startAgent().then(
        (agent) => {
          this.agent = agent;
          this.agentStarting = undefined;
          return agent;
        },
        (err) => {
          this.agentStarting = undefined;
          throw err;
        },
      );
    }
    return this.agentStarting;
  }

  private async startAgent(): Promise<AgentConnection> {
    const workspace = vscode.workspace.workspaceFolders?.[0];
    if (!workspace) {
      throw new Error("Open a folder first — code-assistant needs a workspace to work in.");
    }
    const config = vscode.workspace.getConfiguration("codeAssistant");
    const command = config.get<string>("commandPath", "code-assistant");
    const extraArgs = config.get<string[]>("extraArgs", []);
    const cwd = workspace.uri.fsPath;

    this.output.appendLine(`starting: ${command} acp (cwd: ${cwd})`);
    return AgentConnection.start({
      command,
      args: ["acp", ...extraArgs],
      cwd,
      events: {
        sessionUpdate: (update) => this.post({ type: "sessionUpdate", update }),
        turnEnded: (stopReason) => this.post({ type: "turnEnded", stopReason }),
        promptError: (message) => this.post({ type: "error", message }),
        requestPermission: (params) => this.requestPermission(params),
        readTextFile: (params) => this.readTextFile(params),
        writeTextFile: (params) => this.writeTextFile(params),
        stderr: (chunk) => this.output.append(chunk),
        exited: (code) => {
          this.output.appendLine(`code-assistant exited with code ${code}`);
          this.agent = undefined;
          this.post({ type: "error", message: `code-assistant exited (code ${code})` });
        },
      },
    });
  }

  private async requestPermission(
    params: acp.RequestPermissionRequest,
  ): Promise<acp.RequestPermissionResponse> {
    const title = params.toolCall?.title ?? "Tool call";
    const picked = await vscode.window.showInformationMessage(
      `code-assistant asks for permission:\n\n${title}`,
      { modal: true },
      ...params.options.map((o) => o.name),
    );
    const option = params.options.find((o) => o.name === picked);
    return {
      outcome: option
        ? { outcome: "selected", optionId: option.optionId }
        : { outcome: "cancelled" },
    };
  }

  private async readTextFile(
    params: acp.ReadTextFileRequest,
  ): Promise<acp.ReadTextFileResponse> {
    const doc = await vscode.workspace.openTextDocument(vscode.Uri.file(params.path));
    let content = doc.getText();
    if (params.line != null || params.limit != null) {
      const lines = content.split("\n");
      const start = Math.max((params.line ?? 1) - 1, 0);
      const end = params.limit != null ? start + params.limit : lines.length;
      content = lines.slice(start, end).join("\n");
    }
    return { content };
  }

  private async writeTextFile(
    params: acp.WriteTextFileRequest,
  ): Promise<acp.WriteTextFileResponse> {
    const uri = vscode.Uri.file(params.path);
    const openDoc = vscode.workspace.textDocuments.find(
      (d) => d.uri.fsPath === uri.fsPath,
    );
    if (openDoc) {
      const edit = new vscode.WorkspaceEdit();
      const fullRange = new vscode.Range(
        openDoc.positionAt(0),
        openDoc.positionAt(openDoc.getText().length),
      );
      edit.replace(uri, fullRange, params.content);
      await vscode.workspace.applyEdit(edit);
      await openDoc.save();
    } else {
      await vscode.workspace.fs.writeFile(uri, Buffer.from(params.content, "utf8"));
    }
    return {};
  }

  private post(msg: WebviewOutbound): void {
    void this.view?.webview.postMessage(msg);
  }

  private renderHtml(webview: vscode.Webview): string {
    const scriptUri = webview.asWebviewUri(
      vscode.Uri.joinPath(this.extensionUri, "dist", "webview.js"),
    );
    const styleUri = webview.asWebviewUri(
      vscode.Uri.joinPath(this.extensionUri, "dist", "webview.css"),
    );
    const nonce = crypto.randomUUID().replaceAll("-", "");
    return /* html */ `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="Content-Security-Policy"
        content="default-src 'none'; style-src ${webview.cspSource}; script-src 'nonce-${nonce}';">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <link href="${styleUri}" rel="stylesheet">
</head>
<body>
  <main id="transcript"></main>
  <footer id="composer">
    <div id="plan-area" hidden></div>
    <div id="input-shell">
      <textarea id="input" rows="1" placeholder="Message code-assistant…"></textarea>
      <div id="input-toolbar">
        <div id="config-selectors"></div>
        <span class="spacer"></span>
        <span id="usage-ring"></span>
        <button id="send" title="Send"></button>
      </div>
    </div>
  </footer>
  <script nonce="${nonce}" src="${scriptUri}"></script>
</body>
</html>`;
  }
}
