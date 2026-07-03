import {
  Bot,
  Brain,
  ChevronDown,
  FileOutput,
  FilePen,
  FileSearch,
  Globe,
  LoaderCircle,
  Search,
  Terminal,
  Trash2,
  Wrench,
} from "lucide-static";

const byKind: Record<string, string> = {
  read: FileSearch,
  edit: FilePen,
  delete: Trash2,
  move: FileOutput,
  search: Search,
  execute: Terminal,
  think: Brain,
  fetch: Globe,
  other: Bot,
};

export function toolIcon(kind: string | undefined): string {
  return byKind[kind ?? ""] ?? Wrench;
}

export const brainIcon = Brain;
export const chevronIcon = ChevronDown;
export const spinnerIcon = LoaderCircle;
