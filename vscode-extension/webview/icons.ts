import {
  Bot,
  Brain,
  ChevronDown,
  Circle,
  CircleCheck,
  CloudDownload,
  FileOutput,
  FilePen,
  FileSearch,
  FileText,
  FolderSearch,
  FolderTree,
  Globe,
  Image,
  ListTodo,
  ListTree,
  LoaderCircle,
  MessageCircleQuestion,
  RefreshCw,
  Search,
  SendHorizontal,
  Sparkles,
  Square,
  SquarePen,
  SquareTerminal,
  Tag,
  Trash2,
  Wrench,
} from "lucide-static";

/** Icons for code-assistant tools, resolved from the `_meta` tool name. */
const byToolName: Record<string, string> = {
  read_files: FileSearch,
  read_skill: Sparkles,
  list_files: FolderSearch,
  list_projects: FolderTree,
  list_skills: Sparkles,
  glob_files: ListTree,
  search_files: Search,
  web_search: Globe,
  web_fetch: CloudDownload,
  perplexity_ask: MessageCircleQuestion,
  execute_command: SquareTerminal,
  edit: SquarePen,
  replace_in_file: SquarePen,
  write_file: SquarePen,
  delete_files: Trash2,
  spawn_agent: RefreshCw,
  update_plan: ListTodo,
  name_session: Tag,
  view_documents: FileText,
  view_images: Image,
};

/** Fallback icons by ACP tool kind, for agents that don't send a tool name. */
const byKind: Record<string, string> = {
  read: FileSearch,
  edit: SquarePen,
  delete: Trash2,
  move: FileOutput,
  search: Search,
  execute: SquareTerminal,
  think: Brain,
  fetch: Globe,
  other: Bot,
};

export function toolIcon(toolName: string | undefined, kind: string | undefined): string {
  return byToolName[toolName ?? ""] ?? byKind[kind ?? ""] ?? Wrench;
}

export const brainIcon = Brain;
export const chevronIcon = ChevronDown;
export const spinnerIcon = LoaderCircle;
export const sendIcon = SendHorizontal;
export const stopIcon = Square;

const planStatus: Record<string, string> = {
  pending: Circle,
  in_progress: RefreshCw,
  completed: CircleCheck,
};

export function planStatusIcon(status: string): string {
  return planStatus[status] ?? Circle;
}
