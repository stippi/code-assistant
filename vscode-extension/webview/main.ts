import "./style.css";
import type {
  PlanEntry,
  SessionConfigOption,
  SessionUpdate,
  StopReason,
  ToolCallContent,
} from "@agentclientprotocol/sdk";
import { diffLines } from "diff";
import {
  brainIcon,
  chevronIcon,
  planStatusIcon,
  sendIcon,
  spinnerIcon,
  stopIcon,
  toolIcon,
} from "./icons";
import { renderMarkdown } from "./markdown";

declare function acquireVsCodeApi(): {
  postMessage(msg: unknown): void;
};

type OutboundMessage =
  | { type: "ready" }
  | { type: "prompt"; text: string }
  | { type: "cancel" }
  | { type: "setConfigOption"; configId: string; value: string };

type InboundMessage =
  | { type: "sessionUpdate"; update: SessionUpdate }
  | { type: "turnEnded"; stopReason: StopReason }
  | { type: "configOptions"; options: SessionConfigOption[] }
  | { type: "error"; message: string }
  | { type: "reset" };

const vscode = acquireVsCodeApi();

const transcript = document.getElementById("transcript") as HTMLElement;
const planArea = document.getElementById("plan-area") as HTMLElement;
const inputShell = document.getElementById("input-shell") as HTMLElement;
const input = document.getElementById("input") as HTMLTextAreaElement;
const configSelectors = document.getElementById("config-selectors") as HTMLElement;
const sendButton = document.getElementById("send") as HTMLButtonElement;

let busy = false;

function post(msg: OutboundMessage): void {
  vscode.postMessage(msg);
}

function setBusy(value: boolean): void {
  busy = value;
  sendButton.innerHTML = busy ? stopIcon : sendIcon;
  sendButton.title = busy ? "Stop" : "Send";
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
    openBlock = variant === "agent" ? createAgentBlock() : createThoughtBlock();
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
  iconEl: HTMLElement;
  titleEl: HTMLElement;
  durationEl: HTMLElement;
  trailingEl: HTMLElement;
  bodyEl: HTMLElement;
  toolName?: string;
  kind?: string;
  status?: string;
  content: ToolCallContent[] | null;
  rawInput: unknown;
  rendered: boolean;
  isCard: boolean;
  startedAt: number;
}

const toolCalls = new Map<string, ToolCallView>();

function readToolName(update: unknown): string | undefined {
  const meta = (update as { _meta?: Record<string, unknown> })._meta;
  const extension = meta?.["code-assistant"] as { toolName?: unknown } | undefined;
  return typeof extension?.toolName === "string" ? extension.toolName : undefined;
}

function wantsCard(view: ToolCallView): boolean {
  return (
    view.kind === "edit" ||
    view.kind === "execute" ||
    view.toolName === "spawn_agent"
  );
}

function upsertToolCall(
  update: {
    toolCallId: string;
    title?: string | null;
    kind?: string | null;
    status?: string | null;
    content?: ToolCallContent[] | null;
    rawInput?: unknown;
  },
  toolName: string | undefined,
): void {
  closeOpenBlock();
  let view = toolCalls.get(update.toolCallId);
  if (!view) {
    view = createToolCallView();
    toolCalls.set(update.toolCallId, view);
  }
  if (toolName) {
    view.toolName = toolName;
  }
  if (update.kind) {
    view.kind = update.kind;
  }
  if (update.title) {
    view.titleEl.textContent = update.title;
  }
  if (update.status) {
    view.status = update.status;
    view.card.dataset.status = update.status;
    const running = update.status === "pending" || update.status === "in_progress";
    view.trailingEl.innerHTML = running ? spinnerIcon : chevronIcon;
    if (!running && view.kind === "execute") {
      const seconds = Math.round((Date.now() - view.startedAt) / 1000);
      if (seconds >= 1) {
        view.durationEl.textContent = `(${seconds}s)`;
      }
    }
  }
  if (update.content !== undefined && update.content !== null) {
    view.content = update.content;
    view.rendered = false;
  }
  if (update.rawInput !== undefined) {
    view.rawInput = update.rawInput;
    if (view.content === null) {
      view.rendered = false;
    }
  }
  applyToolStyle(view);
  if (!view.bodyEl.hidden && !view.rendered) {
    renderToolBody(view);
  }
  scrollToBottom();
}

function createToolCallView(): ToolCallView {
  const card = appendBlock("tool-call");
  const header = document.createElement("button");
  header.className = "tool-header";
  header.innerHTML = `<span class="tool-icon"></span><span class="tool-title"></span><span class="trailing"><span class="tool-duration"></span>${chevronIcon}</span>`;
  const body = document.createElement("div");
  body.className = "tool-body";
  body.hidden = true;
  card.append(header, body);

  const view: ToolCallView = {
    card,
    iconEl: header.querySelector(".tool-icon") as HTMLElement,
    titleEl: header.querySelector(".tool-title") as HTMLElement,
    durationEl: header.querySelector(".tool-duration") as HTMLElement,
    trailingEl: header.querySelector(".trailing") as HTMLElement,
    bodyEl: body,
    content: null,
    rawInput: undefined,
    rendered: false,
    isCard: false,
    startedAt: Date.now(),
  };
  header.addEventListener("click", () => {
    body.hidden = !body.hidden;
    card.classList.toggle("expanded", !body.hidden);
    if (!body.hidden && !view.rendered) {
      renderToolBody(view);
    }
  });
  return view;
}

function applyToolStyle(view: ToolCallView): void {
  view.iconEl.innerHTML = toolIcon(view.toolName, view.kind);
  const becomesCard = wantsCard(view);
  if (becomesCard && !view.isCard) {
    view.isCard = true;
    view.card.classList.add("tool-card");
    // Cards show their content without needing a click.
    view.bodyEl.hidden = false;
    view.card.classList.add("expanded");
  }
  view.card.classList.toggle("tool-card-edit", view.isCard && view.kind === "edit");
  view.card.classList.toggle(
    "tool-card-terminal",
    view.isCard && view.kind === "execute",
  );
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
    case "diff":
      return renderDiff(item.oldText ?? "", item.newText);
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

function renderDiff(oldText: string, newText: string): HTMLElement {
  const container = document.createElement("div");
  container.className = "diff";
  for (const part of diffLines(oldText, newText)) {
    const lines = part.value.split("\n");
    if (lines[lines.length - 1] === "") {
      lines.pop();
    }
    for (const line of lines) {
      const row = document.createElement("div");
      row.className = part.added
        ? "diff-line diff-added"
        : part.removed
          ? "diff-line diff-removed"
          : "diff-line";
      const gutter = document.createElement("span");
      gutter.className = "diff-gutter";
      gutter.textContent = part.added ? "+" : part.removed ? "−" : " ";
      const content = document.createElement("span");
      content.className = "diff-content";
      content.textContent = line;
      row.append(gutter, content);
      container.appendChild(row);
    }
  }
  return container;
}

// --- session config options (model selector etc.) ---

function renderConfigOptions(options: SessionConfigOption[]): void {
  configSelectors.innerHTML = "";
  for (const option of options) {
    if (option.type !== "select") {
      continue;
    }
    const select = document.createElement("select");
    select.className = "config-select";
    select.title = option.name;
    for (const entry of option.options) {
      if ("group" in entry) {
        const group = document.createElement("optgroup");
        group.label = entry.name;
        for (const item of entry.options) {
          group.appendChild(new Option(item.name, item.value));
        }
        select.appendChild(group);
      } else {
        select.appendChild(new Option(entry.name, entry.value));
      }
    }
    select.value = option.currentValue;
    select.addEventListener("change", () => {
      post({ type: "setConfigOption", configId: option.id, value: select.value });
    });
    configSelectors.appendChild(select);
  }
}

// --- plan ---

let planCollapsed = false;

function renderPlan(entries: PlanEntry[]): void {
  planArea.innerHTML = "";
  planArea.hidden = entries.length === 0;
  if (entries.length === 0) {
    return;
  }

  const completed = entries.filter((e) => e.status === "completed").length;
  const current = entries.find((e) => e.status === "in_progress");

  const header = document.createElement("button");
  header.className = planCollapsed ? "plan-header" : "plan-header expanded";
  const currentLabel =
    planCollapsed && current
      ? `<span class="plan-current">${planStatusIcon("in_progress")}<span></span></span>`
      : "";
  header.innerHTML = `<span class="plan-chevron">${chevronIcon}</span><span class="plan-title">Plan</span>${currentLabel}<span class="plan-count">${completed}/${entries.length}</span>`;
  if (planCollapsed && current) {
    (header.querySelector(".plan-current span") as HTMLElement).textContent =
      current.content;
  }
  header.addEventListener("click", () => {
    planCollapsed = !planCollapsed;
    renderPlan(entries);
  });
  planArea.appendChild(header);

  if (!planCollapsed) {
    const list = document.createElement("div");
    list.className = "plan-entries";
    for (const entry of entries) {
      const row = document.createElement("div");
      row.className = `plan-entry ${entry.status}`;
      const icon = document.createElement("span");
      icon.className = "plan-entry-icon";
      icon.innerHTML = planStatusIcon(entry.status);
      const content = document.createElement("span");
      content.className = "plan-entry-content";
      content.textContent = entry.content;
      row.append(icon, content);
      list.appendChild(row);
    }
    planArea.appendChild(list);
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
      upsertToolCall(update, readToolName(update));
      break;
    case "plan":
      renderPlan(update.entries);
      break;
    case "plan_removed":
      renderPlan([]);
      break;
    case "config_option_update":
      renderConfigOptions(update.configOptions);
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
input.addEventListener("input", () => {
  input.style.height = "auto";
  input.style.height = `${Math.min(input.scrollHeight, 160)}px`;
});
// The whole shell acts as the input surface.
inputShell.addEventListener("click", (e) => {
  if (e.target === inputShell || e.target === input.parentElement) {
    input.focus();
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
    case "configOptions":
      renderConfigOptions(msg.options);
      break;
    case "reset":
      transcript.innerHTML = "";
      toolCalls.clear();
      closeOpenBlock();
      renderPlan([]);
      configSelectors.innerHTML = "";
      setBusy(false);
      break;
  }
});

setBusy(false);
post({ type: "ready" });
