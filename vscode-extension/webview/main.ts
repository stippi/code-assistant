import "./style.css";
import type {
  SessionUpdate,
  StopReason,
  ToolCallContent,
} from "@agentclientprotocol/sdk";
import { brainIcon, chevronIcon, spinnerIcon, toolIcon } from "./icons";
import { renderMarkdown } from "./markdown";

declare function acquireVsCodeApi(): {
  postMessage(msg: unknown): void;
};

type OutboundMessage =
  | { type: "prompt"; text: string }
  | { type: "cancel" };

type InboundMessage =
  | { type: "sessionUpdate"; update: SessionUpdate }
  | { type: "turnEnded"; stopReason: StopReason }
  | { type: "error"; message: string }
  | { type: "reset" };

const vscode = acquireVsCodeApi();

const transcript = document.getElementById("transcript") as HTMLElement;
const input = document.getElementById("input") as HTMLTextAreaElement;
const sendButton = document.getElementById("send") as HTMLButtonElement;

let busy = false;

function post(msg: OutboundMessage): void {
  vscode.postMessage(msg);
}

function setBusy(value: boolean): void {
  busy = value;
  sendButton.textContent = value ? "Stop" : "Send";
}

function scrollToBottom(): void {
  transcript.scrollTop = transcript.scrollHeight;
}

function appendBlock(className: string): HTMLElement {
  const el = document.createElement("div");
  el.className = className;
  transcript.appendChild(el);
  scrollToBottom();
  return el;
}

// --- streaming text blocks (agent messages and thoughts) ---

interface TextBlock {
  variant: "agent" | "thought";
  raw: string;
  contentEl: HTMLElement;
  labelEl?: HTMLElement;
  startedAt: number;
}

let openBlock: TextBlock | null = null;

function closeOpenBlock(): void {
  if (openBlock?.variant === "thought" && openBlock.labelEl) {
    const seconds = Math.max(1, Math.round((Date.now() - openBlock.startedAt) / 1000));
    openBlock.labelEl.textContent = `Thought for ${seconds}s`;
  }
  openBlock = null;
}

function appendStreamText(variant: "agent" | "thought", text: string): void {
  if (openBlock?.variant !== variant) {
    closeOpenBlock();
    openBlock =
      variant === "agent" ? createAgentBlock() : createThoughtBlock();
  }
  openBlock.raw += text;
  renderMarkdown(openBlock.contentEl, openBlock.raw);
  scrollToBottom();
}

function createAgentBlock(): TextBlock {
  const el = appendBlock("agent-message markdown");
  return { variant: "agent", raw: "", contentEl: el, startedAt: Date.now() };
}

function createThoughtBlock(): TextBlock {
  const card = appendBlock("thought-card");
  const header = document.createElement("button");
  header.className = "thought-header";
  header.innerHTML = `${brainIcon}<span class="thought-label">Thinking…</span><span class="trailing">${chevronIcon}</span>`;
  const body = document.createElement("div");
  body.className = "thought-body markdown";
  body.hidden = true;
  header.addEventListener("click", () => {
    body.hidden = !body.hidden;
    card.classList.toggle("expanded", !body.hidden);
  });
  card.append(header, body);
  return {
    variant: "thought",
    raw: "",
    contentEl: body,
    labelEl: header.querySelector(".thought-label") as HTMLElement,
    startedAt: Date.now(),
  };
}

// --- tool calls ---

interface ToolCallView {
  card: HTMLElement;
  titleEl: HTMLElement;
  trailingEl: HTMLElement;
  bodyEl: HTMLElement;
  content: ToolCallContent[] | null;
  rawInput: unknown;
  rendered: boolean;
}

const toolCalls = new Map<string, ToolCallView>();

function upsertToolCall(update: {
  toolCallId: string;
  title?: string | null;
  kind?: string | null;
  status?: string | null;
  content?: ToolCallContent[] | null;
  rawInput?: unknown;
}): void {
  closeOpenBlock();
  let view = toolCalls.get(update.toolCallId);
  if (!view) {
    view = createToolCallView(update.kind ?? "other");
    toolCalls.set(update.toolCallId, view);
  }
  if (update.title) {
    view.titleEl.textContent = update.title;
  }
  if (update.kind) {
    const icon = view.card.querySelector(".tool-icon");
    if (icon) {
      icon.innerHTML = toolIcon(update.kind);
    }
    view.card.classList.toggle("tool-agent-card", update.kind === "other");
  }
  if (update.status) {
    view.card.dataset.status = update.status;
    const running = update.status === "pending" || update.status === "in_progress";
    view.trailingEl.innerHTML = running ? spinnerIcon : chevronIcon;
  }
  if (update.content !== undefined && update.content !== null) {
    view.content = update.content;
    view.rendered = false;
  }
  if (update.rawInput !== undefined) {
    view.rawInput = update.rawInput;
    view.rendered = view.rendered && view.content !== null;
  }
  if (!view.bodyEl.hidden && !view.rendered) {
    renderToolBody(view);
  }
  scrollToBottom();
}

function createToolCallView(kind: string): ToolCallView {
  const card = appendBlock(kind === "other" ? "tool-call tool-agent-card" : "tool-call");
  const header = document.createElement("button");
  header.className = "tool-header";
  header.innerHTML = `<span class="tool-icon">${toolIcon(kind)}</span><span class="tool-title"></span><span class="trailing">${chevronIcon}</span>`;
  const body = document.createElement("div");
  body.className = "tool-body";
  body.hidden = true;
  card.append(header, body);

  const view: ToolCallView = {
    card,
    titleEl: header.querySelector(".tool-title") as HTMLElement,
    trailingEl: header.querySelector(".trailing") as HTMLElement,
    bodyEl: body,
    content: null,
    rawInput: undefined,
    rendered: false,
  };
  header.addEventListener("click", () => {
    body.hidden = !body.hidden;
    card.classList.toggle("expanded", !body.hidden);
    if (!body.hidden && !view.rendered) {
      renderToolBody(view);
    }
  });
  // Agent-style cards show their content without needing a click.
  if (kind === "other") {
    body.hidden = false;
    card.classList.add("expanded");
  }
  return view;
}

function renderToolBody(view: ToolCallView): void {
  view.bodyEl.innerHTML = "";
  if (view.content && view.content.length > 0) {
    for (const item of view.content) {
      view.bodyEl.appendChild(renderToolContent(item));
    }
  } else if (view.rawInput !== undefined) {
    const pre = document.createElement("pre");
    pre.className = "tool-output";
    pre.textContent = JSON.stringify(view.rawInput, null, 2);
    view.bodyEl.appendChild(pre);
  } else {
    const empty = document.createElement("div");
    empty.className = "tool-empty";
    empty.textContent = "No output yet";
    view.bodyEl.appendChild(empty);
  }
  view.rendered = true;
}

function renderToolContent(item: ToolCallContent): HTMLElement {
  switch (item.type) {
    case "content": {
      const pre = document.createElement("pre");
      pre.className = "tool-output";
      pre.textContent =
        item.content.type === "text" ? item.content.text : `[${item.content.type}]`;
      return pre;
    }
    case "diff": {
      const container = document.createElement("div");
      container.className = "diff";
      const path = document.createElement("div");
      path.className = "diff-path";
      path.textContent = item.path;
      container.appendChild(path);
      if (item.oldText) {
        const removed = document.createElement("pre");
        removed.className = "diff-removed";
        removed.textContent = item.oldText;
        container.appendChild(removed);
      }
      const added = document.createElement("pre");
      added.className = "diff-added";
      added.textContent = item.newText;
      container.appendChild(added);
      return container;
    }
    case "terminal": {
      const el = document.createElement("div");
      el.className = "tool-empty";
      el.textContent = `[terminal ${item.terminalId}]`;
      return el;
    }
    default: {
      const el = document.createElement("div");
      el.className = "tool-empty";
      el.textContent = "[unsupported content]";
      return el;
    }
  }
}

// --- session updates ---

function handleUpdate(update: SessionUpdate): void {
  switch (update.sessionUpdate) {
    case "agent_message_chunk":
      if (update.content.type === "text") {
        appendStreamText("agent", update.content.text);
      }
      break;
    case "agent_thought_chunk":
      if (update.content.type === "text") {
        appendStreamText("thought", update.content.text);
      }
      break;
    case "tool_call":
    case "tool_call_update":
      upsertToolCall(update);
      break;
    case "user_message_chunk":
      // The composer already rendered the user's message.
      break;
    default:
      break;
  }
}

function submit(): void {
  if (busy) {
    post({ type: "cancel" });
    return;
  }
  const text = input.value.trim();
  if (!text) {
    return;
  }
  input.value = "";
  closeOpenBlock();
  appendBlock("user-message").textContent = text;
  setBusy(true);
  post({ type: "prompt", text });
}

sendButton.addEventListener("click", submit);
input.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    submit();
  }
});

window.addEventListener("message", (event: MessageEvent<InboundMessage>) => {
  const msg = event.data;
  switch (msg.type) {
    case "sessionUpdate":
      handleUpdate(msg.update);
      break;
    case "turnEnded":
      closeOpenBlock();
      setBusy(false);
      break;
    case "error":
      closeOpenBlock();
      appendBlock("error-message").textContent = msg.message;
      setBusy(false);
      break;
    case "reset":
      transcript.innerHTML = "";
      toolCalls.clear();
      closeOpenBlock();
      setBusy(false);
      break;
  }
});
