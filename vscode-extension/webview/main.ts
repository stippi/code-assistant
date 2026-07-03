import "./style.css";
import type { SessionUpdate, StopReason } from "@agentclientprotocol/sdk";

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
// Text chunks append to this element until a non-text update interrupts the flow.
let openTextBlock: HTMLElement | null = null;
const toolCalls = new Map<string, HTMLElement>();

function post(msg: OutboundMessage): void {
  vscode.postMessage(msg);
}

function setBusy(value: boolean): void {
  busy = value;
  sendButton.textContent = value ? "Stop" : "Send";
}

function appendBlock(className: string): HTMLElement {
  const el = document.createElement("div");
  el.className = className;
  transcript.appendChild(el);
  transcript.scrollTop = transcript.scrollHeight;
  return el;
}

function appendText(className: string, text: string): void {
  if (!openTextBlock || openTextBlock.className !== className) {
    openTextBlock = appendBlock(className);
  }
  openTextBlock.textContent += text;
  transcript.scrollTop = transcript.scrollHeight;
}

function closeTextBlock(): void {
  openTextBlock = null;
}

function renderToolCall(
  toolCallId: string,
  title: string | undefined,
  status: string | undefined,
): void {
  closeTextBlock();
  let el = toolCalls.get(toolCallId);
  if (!el) {
    el = appendBlock("tool-call");
    el.innerHTML = `<span class="tool-status"></span><span class="tool-title"></span>`;
    toolCalls.set(toolCallId, el);
  }
  if (title) {
    el.querySelector(".tool-title")!.textContent = title;
  }
  if (status) {
    el.querySelector(".tool-status")!.className = `tool-status ${status}`;
  }
}

function handleUpdate(update: SessionUpdate): void {
  switch (update.sessionUpdate) {
    case "agent_message_chunk":
      if (update.content.type === "text") {
        appendText("agent-message", update.content.text);
      }
      break;
    case "agent_thought_chunk":
      if (update.content.type === "text") {
        appendText("agent-thought", update.content.text);
      }
      break;
    case "tool_call":
      renderToolCall(update.toolCallId, update.title, update.status);
      break;
    case "tool_call_update":
      renderToolCall(
        update.toolCallId,
        update.title ?? undefined,
        update.status ?? undefined,
      );
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
  closeTextBlock();
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
      closeTextBlock();
      setBusy(false);
      break;
    case "error":
      closeTextBlock();
      appendBlock("error-message").textContent = msg.message;
      setBusy(false);
      break;
    case "reset":
      transcript.innerHTML = "";
      toolCalls.clear();
      closeTextBlock();
      setBusy(false);
      break;
  }
});
