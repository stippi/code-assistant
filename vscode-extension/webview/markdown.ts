import DOMPurify from "dompurify";
import { marked } from "marked";

marked.setOptions({ gfm: true, breaks: false });

export function renderMarkdown(target: HTMLElement, source: string): void {
  target.innerHTML = DOMPurify.sanitize(marked.parse(source, { async: false }));
}
