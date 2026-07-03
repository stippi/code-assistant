import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { Readable, Writable } from "node:stream";
import * as acp from "@agentclientprotocol/sdk";

export interface AgentEvents {
  sessionUpdate(update: acp.SessionUpdate): void;
  turnEnded(stopReason: acp.StopReason): void;
  promptError(message: string): void;
  requestPermission(
    params: acp.RequestPermissionRequest,
  ): Promise<acp.RequestPermissionResponse>;
  readTextFile(params: acp.ReadTextFileRequest): Promise<acp.ReadTextFileResponse>;
  writeTextFile(params: acp.WriteTextFileRequest): Promise<acp.WriteTextFileResponse>;
  stderr(chunk: string): void;
  exited(code: number | null): void;
}

export interface AgentStartOptions {
  command: string;
  args: string[];
  cwd: string;
  events: AgentEvents;
}

/**
 * Owns one `code-assistant acp` child process with a single active session.
 */
export class AgentConnection {
  private constructor(
    private readonly child: ChildProcessWithoutNullStreams,
    private readonly connection: acp.ClientConnection,
    private readonly session: acp.ActiveSession,
    private readonly events: AgentEvents,
  ) {
    this.pumpUpdates();
  }

  static async start(options: AgentStartOptions): Promise<AgentConnection> {
    const { command, args, cwd, events } = options;

    const child = spawn(command, args, { cwd, stdio: ["pipe", "pipe", "pipe"] });
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk) => events.stderr(String(chunk)));

    const startFailure = new Promise<never>((_, reject) => {
      child.once("error", (err) =>
        reject(new Error(`failed to start \`${command}\`: ${err.message}`)),
      );
      child.once("exit", (code) =>
        reject(new Error(`\`${command}\` exited with code ${code} during startup`)),
      );
    });
    // Suppress unhandled-rejection noise once startup has succeeded.
    startFailure.catch(() => {});

    const stream = acp.ndJsonStream(
      Writable.toWeb(child.stdin),
      Readable.toWeb(child.stdout),
    );

    const connection = acp
      .client({ name: "code-assistant-vscode" })
      .onRequest(acp.methods.client.session.requestPermission, (ctx) =>
        events.requestPermission(ctx.params),
      )
      .onRequest(acp.methods.client.fs.readTextFile, (ctx) =>
        events.readTextFile(ctx.params),
      )
      .onRequest(acp.methods.client.fs.writeTextFile, (ctx) =>
        events.writeTextFile(ctx.params),
      )
      .connect(stream);

    try {
      await Promise.race([
        connection.agent.request(acp.methods.agent.initialize, {
          protocolVersion: acp.PROTOCOL_VERSION,
          clientCapabilities: {
            fs: { readTextFile: true, writeTextFile: true },
          },
        }),
        startFailure,
      ]);
      const session = await Promise.race([
        connection.agent.buildSession(cwd).start(),
        startFailure,
      ]);
      child.removeAllListeners("exit");
      child.removeAllListeners("error");
      child.on("exit", (code) => events.exited(code));
      return new AgentConnection(child, connection, session, events);
    } catch (err) {
      child.kill();
      throw err;
    }
  }

  get sessionId(): string {
    return this.session.sessionId;
  }

  get configOptions(): acp.SessionConfigOption[] {
    return this.session.newSessionResponse.configOptions ?? [];
  }

  async setConfigOption(
    configId: string,
    value: string,
  ): Promise<acp.SessionConfigOption[]> {
    const response = await this.connection.agent.request(
      acp.methods.agent.session.setConfigOption,
      { sessionId: this.session.sessionId, configId, value },
    );
    return response.configOptions;
  }

  prompt(text: string): void {
    // Turn completion is reported through the update pump; the promise only
    // matters when the request itself fails.
    void this.session.prompt(text).catch((err: unknown) => {
      this.events.promptError(err instanceof Error ? err.message : String(err));
    });
  }

  async cancel(): Promise<void> {
    await this.connection.agent.notify(acp.methods.agent.session.cancel, {
      sessionId: this.session.sessionId,
    });
  }

  dispose(): void {
    this.session.dispose();
    this.child.kill();
  }

  private pumpUpdates(): void {
    void (async () => {
      try {
        for (;;) {
          const message = await this.session.nextUpdate();
          if (message.kind === "stop") {
            this.events.turnEnded(message.stopReason);
          } else {
            this.events.sessionUpdate(message.update);
          }
        }
      } catch {
        // Stream closed; the child's exit handler reports termination.
      }
    })();
  }
}
