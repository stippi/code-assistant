# `ask_question` Tool

Lets the agent ask the user up to **4 multiple-choice questions** at once and
wait for the answers. Each question has 2–4 options; `multi_select` switches
from radio buttons to checkboxes. Every question also gets a free-text comment
field, so the agent should not add an "Other" option.

The tool result lists, per question, the selected option labels and the
comment. Skipping the form returns a "declined" result; stopping the agent
cancels the request.

## Availability

Only the GPUI frontend offers the tool: it is registered through
`tools::register_interactive_tools`, which the GPUI wiring includes
(`ConfigToolRegistry::new_interactive`, `default_registry_for(true)`).
Terminal, ACP and the MCP server build registries without it, so the agent
never waits on a prompt nobody sees. It is tagged `scope:agent`, so
sub-agents don't get it either.

## Architecture

Mirrors the permission prompt flow (`docs/permission-tiers.md`):

- **Types + pending store**: `code_assistant_core::session::questions`
  (`UserQuestion`, `QuestionAnswer`, `UserQuestionRequest`,
  `PendingQuestions`). Open requests live on the `SessionInstance`, are
  included in `SessionSnapshot::pending_questions`, and are cancelled by
  `request_stop` and at the start of a new run.
- **Transport**: the tool calls `UserInterface::ask_questions` (default:
  unsupported). `SessionEventPublisher` implements it by publishing
  `UiEvent::RequestUserQuestions` and awaiting
  `SessionService::answer_questions` (`Some(answers)` or `None` to decline);
  `UiEvent::UserQuestionsResolved` follows once settled.
- **GPUI**: `Gpui` tracks open requests per asking session (the sidebar flags
  that session like a pending permission); `main_screen/question_prompt.rs`
  renders the oldest request above the input with radios/checkboxes, a
  comment input per question, and *Skip* / *Submit answers*.
