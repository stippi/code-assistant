# Roadmap

This roadmap describes the direction of code-assistant as both a coding agent
and a reusable agent platform. It is intentionally organized around domain
ownership and lifecycle semantics rather than UI features. Dates and ordering
are provisional; the architectural boundaries and invariants are the important
part.

PAL is the first substantial downstream consumer of this stack. It has already
demonstrated that projects, goals, plans, delegated runs, skills, evidence, and
programmatic tool composition are not personal-assistant-specific concepts.
They are useful to an interactive coding agent as well. PAL should specialize
how those concepts are hosted continuously, not maintain incompatible copies of
their generic semantics.

## Product boundary

The useful distinction between code-assistant and PAL is the deployment and
continuation contract, not whether an agent may have goals, projects, or child
agents.

### code-assistant owns the reusable agent platform

code-assistant should own generic concepts that are useful in an interactive
CLI, GUI, editor integration, MCP server, or another embedding application:

- the model/tool agent loop and its extension points;
- session lifecycle, persistence, branching, event streaming, and permissions;
- tool implementations and capability-scoped tool registries;
- projects, project instructions, worktrees, and execution context;
- plans, durable goals, completion contracts, budgets, and evidence;
- isolated child agents and the generic lifecycle of a run attempt;
- optional work graphs for dependency-aware multi-agent collaboration;
- skills and other reusable procedures;
- programmatic tool composition ("code mode");
- generic wait conditions, approval records, and execution-target contracts.

code-assistant may persist these objects and restore them when it is launched.
It does not have to promise that every unfinished object is proactively resumed
while no code-assistant host is running.

### PAL owns the always-on personal-agent host

PAL should own behavior whose meaning comes from being a persistent personal
service:

- stable channel/lane identity across Telegram and future gateways;
- automatic selection and continuation of the session for an incoming channel;
- daily/session-incarnation rotation without losing the user's durable intent;
- startup reconciliation and proactive restart of interrupted autonomous work;
- schedules, external triggers, delivery routing, and a durable outbox;
- headless supervision, health reporting, and NUC service operation;
- personal memory tiers, ambient observation, dreaming, curation, and forgetting;
- personal policy about what may cross channels, projects, or memory scopes.

PAL may provide stronger persistence and recovery guarantees for shared domain
objects than the interactive code-assistant applications do. That is a host
policy, not a reason for the objects themselves to diverge.

### Shared mechanism, host-supplied policy

Several capabilities sit on the boundary. Their model and contracts should be
shared, while their concrete drivers remain host-specific:

| Shared concept | code-assistant host | PAL host |
| --- | --- | --- |
| Goal | User starts/resumes it in a session; an open app may continue it | Supervisor discovers and drives it after restart across session incarnations |
| Run | Interactive/background child associated with a session | Durable child adopted, retried, or restarted by the gateway supervisor |
| Wait | Process-local or persisted session condition | Durable timer, process, job, child, external event, or human reply barrier |
| Approval | UI/ACP permission mediator | Persisted cross-channel request that may outlive the originating process |
| Project | Coding workspace and defaults | Also a memory, delivery, and personal-policy scope |
| Memory | Project guidance, explicit facts, and retrieval hooks | Cross-session personal memory with aging, dreaming, and truth maintenance |

## Concept model

These concepts overlap in user experience but must have one clear owner each:

```text
Project
  |-- project instructions, policies, skills, and memory scope
  |-- Goal
  |     `-- optional WorkGraph
  |           `-- WorkItem
  |                 `-- one or more RunAttempts
  |                       `-- normal tool calls or code mode
  `-- ordinary sessions and one-shot tasks
```

- A **Project** owns context and policy: folders, instructions, defaults,
  capabilities, memory scope, and worktree conventions. It is archived, not
  "completed".
- A **Goal** owns desired outcome and completion: objective, completion
  contract, verification, boundaries, budget, and terminal verdict.
- A **Plan** is the agent's lightweight, session-local working outline. It may
  be revised freely and has no claim, retry, or completion authority.
- A **WorkGraph** owns optional decomposition and coordination: dependencies,
  assignment, readiness, blocking, and hand-offs. Simple goals do not need one.
- A **WorkItem** owns one locally verifiable unit of coordinated work. Completing
  all work items is evidence for a goal, not an automatic substitute for goal
  evaluation.
- A **RunAttempt** owns one actual execution: model, tools, permissions,
  workspace, timestamps, usage, artifacts, result, and cancellation state.
- A **Skill** owns a reusable procedure. It is a versioned template used by a
  run, not a live workflow instance.
- **Code mode** is an execution mechanism inside one run. It composes tools
  programmatically but owns no durable goal, plan, or retry state.

Automations and external triggers create or wake these same objects. They must
not introduce a second agent runtime or a parallel completion model.

## Crate direction

### Keep `agent_core` small

`agent_core` should remain the application-neutral, single-agent loop:

- conversation and iteration mechanics;
- model streaming and tool dispatch;
- hook, dialect, UI, and persistence traits;
- per-run extension state.

Projects, durable goals, work graphs, schedulers, and channel routing sit above
one agent loop and should not be folded into it merely because they are generic.

### Treat `code_assistant_core` as a shared platform today

PAL's current dependency on `code_assistant_core` is not inherently a layering
failure. That crate already contains the reusable session actor, event stream,
permissions, skills, project manager, tool implementations, browser integration,
and sub-agent runner that PAL needs.

The real problem is that its name and some implementations still assume the
code-assistant product. New shared features should be designed behind host
interfaces instead of reaching into a particular frontend or config directory.

### Extract an orchestration crate when the first shared feature moves

When PAL and code-assistant begin sharing the goal implementation, introduce a
small storage-agnostic crate such as `agent_orchestration` rather than making
`pal_core` depend on all of `code_assistant_core`. The exact name is secondary;
the dependency direction is not:

```text
agent_core             tools_core
     \                    /
      `-- agent_orchestration --'
                 |
        code_assistant_core
                 |
        code-assistant hosts

agent_orchestration is also consumed by pal_core;
PAL supplies its lane, gateway, scheduler, and recovery adapters.
```

Candidate contents:

- goal, completion-contract, budget, verdict, and attempt-ledger types;
- run and work-item identity and lifecycle types;
- store, evaluator, runner, clock, and evidence-source traits;
- typed wait descriptions without concrete gateway/job implementations;
- deterministic state transitions and recovery decisions with exhaustive tests.

It must not depend on a frontend, PAL channels, global config paths, or a
specific JSON/SQLite store. Concrete tools and `SessionService` adapters can
remain in `code_assistant_core`; PAL can provide its own tool presentation and
supervisor adapters.

Do not split more crates preemptively. Extract a seam when both code-assistant
and PAL have a concrete consumer and the dependency direction is demonstrably
clean.

## Now: converge goals and exact turn ownership

> **Status 2026-07-16.** Migration steps 1 and 2 are done: the exact turn
> handle (`start_turn_if_idle` → `TurnHandle` → typed `TurnOutcome`) landed in
> `SessionService`, and the goal/wait domain below lives in
> `agent_orchestration` (PAL consumes it through re-export shims).
> code-assistant is now the second consumer: user-set goals via the `/goal`
> command (`goal_commands`, owner = session; there is deliberately no
> model-facing goal tool, and no system-prompt goal block — a goal reaches
> the model only as the framed goal-turn message), and a
> host-driven `GoalController` (`code_assistant_core::goals`) that drives
> `Running` goals through `start_turn_if_idle` while the app is open —
> deliberately no unattended auto-resume after process end. Remaining from
> this section: step 3 (run/delegation convergence, see Next).

The highest-value shared migration is the generic part of PAL's durable goal
feature. Its state machine already separates deterministic controller policy
from the model-backed evaluator and records bounded attempts instead of model
reasoning. The following pieces belong upstream:

- `Goal`, `GoalState`, `CompletionContract`, `Budget`, `Subgoal`, and verdicts;
- the attempt/evidence ledger and in-flight attempt claim;
- optimistic-revision and attempt-token semantics;
- `GoalEvaluator`, `GoalStore`, `Clock`, and `GoalRunner` traits;
- deterministic controller decisions (`Continue`, `Wait`, `AwaitInput`,
  `Blocked`, `Done`, and `Failed`);
- typed wait barriers (`WaitKind`, wait requests, the armed → satisfied /
  timed-out / cancelled state machine, and clock-only predicates): the
  controller's `Wait` decision folds out of the same state machine, so leaving
  the wait types behind would split that machine across repositories;
- a default bounded LLM evaluator that requires concrete verification evidence;
- generic create/show/pause/resume/cancel/update operations and tool contracts.

The following remain PAL responsibilities:

- binding a goal to `SessionKey` and a delivery channel;
- choosing or rotating the concrete session incarnation that pursues it;
- startup sweeps, orphan adoption, and proactive continuation after restart;
- durable timer/job/child/event/human-input resolvers;
- wait stores, runtime probes, and the turn-free sweep passes;
- cross-channel notifications and final delivery.

### Migration order

1. **Turn handle and structured evidence first.** Both are purely additive
   upstream seams: PAL benefits immediately (goal turns and supervised child
   runs stop inferring outcomes from the event stream) and no PAL code moves
   yet.
2. **Goal and wait types, the controller, and the store/evaluator traits**
   into the orchestration crate. Defining the store traits in this same slice
   matters: PAL plans to replace its per-file JSON stores with one
   transactional repository, and that repository should be written against
   the shared traits rather than migrating persistence twice.
3. **Run/delegation convergence** (see Next) once goals are shared.

### Replace event inference with an exact turn handle

`SessionService::try_send_user_message_if_idle` is the correct atomic dispatch
seam, but its boolean result is not enough for a generic autonomous controller.
The caller still has to infer which activity transition and tool events belong
to the turn it just started.

Add a typed, correlation-safe operation along these lines:

```text
start_turn_if_idle(request) -> Busy | Started(TurnHandle)
TurnHandle::wait()          -> TurnOutcome
```

`TurnHandle` should identify the exact session incarnation and turn, support
cancellation, and resolve once with bounded output:

- final assistant response;
- terminal status and error class;
- tool/resource changes and produced artifacts;
- verification results;
- token/tool-call/wall-time usage;
- whether user input preempted or superseded the turn.

This removes a race from PAL and is independently useful for background agents,
ACP, tests, CLI automation, and future work-graph workers.

Channel-style dispatch (PAL's send-or-queue path) is deliberately not this
operation: an autonomous controller needs exactly `Busy | Started`, while
queueing a user message for later is host delivery policy. If the two paths
ever merge, the operation needs an explicit `Queued` outcome rather than
overloading `Busy`.

### Evidence must be structured at the source

Goal completion must not rely only on the assistant's final narrative. Extend
the run lifecycle with bounded structured evidence emitted by the tool/runtime
path:

- files or resources created, modified, or deleted;
- commands and checks with exit status and a bounded result excerpt;
- explicit artifacts and external references;
- permission decisions and policy violations;
- model, skill versions, tool profile, and execution target used.

The default evaluator may interpret this evidence, but it must never invent it.
Store summaries and references rather than unbounded terminal output or hidden
reasoning.

### Goal migration acceptance criteria

All met as of 2026-07-16:

- ✅ The deterministic goal/controller tests run without an LLM or frontend
  (`agent_orchestration`'s suite plus `code_assistant_core::goals` controller
  tests against scripted providers/evaluators).
- ✅ code-assistant can create and continue a goal in an ordinary session
  (user-set via `/goal` → `goal_commands` + `GoalController`; the earlier
  model-facing `goal` tool was removed — only the user sets goals).
- ✅ A goal may survive session reload without promising unattended
  auto-resume (store-persisted; surfaced on demand via `/goal` list/show;
  the controller loop lives and dies with the process).
- ✅ PAL uses the shared model/controller while retaining its stronger restart
  and channel semantics (re-export shims since 2026-07-16).
- ✅ Concurrent pause/cancel and turn completion cannot overwrite each other
  (revision/claim-token merge in the shared store, pinned by tests in both
  consumers).
- ✅ Every claimed attempt consumes budget or closes with an explicit
  abandonment reason; crashes cannot silently refund work (Busy-abandonment is
  the sole refund path; stale in-flight claims fold as interrupted attempts).

## Next: unify runs and delegation

The current `spawn_agent` path is intentionally useful and fast: it creates an
isolated in-memory agent, blocks the parent tool call, and returns a summary.
It should remain the default for bounded fork/join work.

Its implementation should, however, converge on a generic run description and
result model shared with background and durable execution:

- `RunSpec`: owner, role, instructions, model, tool/capability profile,
  permissions, project/workdir, execution target, budget, and expected output;
- `RunRecord`: stable identity, parent/goal/work-item links, status, timestamps,
  attempt number, usage, artifacts, evidence, and result;
- `RunAttempt`: one claim and execution, distinct from the logical run so a
  retry never rewrites history;
- `AgentRunner`: launch, observe, cancel, and collect a typed result;
- bounded fan-out/depth and explicit nested-delegation policy.

Provide multiple policies over the same primitives:

- **inline fork/join**: today's `spawn_agent`, in-memory and parent-blocking;
- **background session run**: visible in code-assistant and reconnectable while
  the host remains available;
- **durable supervised run**: PAL's store and supervisor can reclaim/restart it;
- **work-graph worker**: claims a ready item and reports evidence to the graph.

Do not make every code-assistant sub-agent durable. Durability has storage,
recovery, UX, and cost consequences and should be selected by the owner.

Budgets in `RunSpec` are owner policy, not a platform mandate: PAL's
supervised children deliberately run to completion, limited only at launch
time (depth, concurrency, workdir ownership, an optional wall-clock deadline),
and the shared types must keep that legal. The run/attempt split, conversely,
is exactly what PAL's `ChildRun` still lacks — converging on it supplies the
planned per-child retry policy and attempt history instead of a parallel
implementation.

## Next: first-class projects

The current project model is essentially one filesystem path plus formatter
configuration. Promote it to the context and policy boundary used by all agent
surfaces:

- stable id, human name, and one or more folders/repositories;
- one primary root and deterministic worktree/branch conventions;
- project instructions and applicable `AGENTS.md`/`CLAUDE.md` sources;
- default model, tool/capability profile, sandbox, and permission policy;
- skill set and explicit project-memory/evidence scope;
- session, goal, run, and work-item references by project id rather than copied
  paths;
- archive semantics that preserve history;
- longest-owned-prefix resolution for sessions opened from a path.

Projects do not own task status. One project can contain many unrelated goals,
one-shot sessions, automations, and work graphs.

## Then: optional work graphs and collaboration

Add a durable DAG only for work that benefits from coordination. A goal should
continue directly through ordinary turns or child runs when it has no meaningful
dependencies or hand-offs.

The work graph should support:

- work items with local verification, priority, assignee capability/role, and
  parent goal or other explicit owner;
- dependency promotion, typed block reasons, comments, artifacts, and evidence;
- atomic claims, leases, heartbeats, stale-claim recovery, bounded retries, and
  circuit breakers;
- human reassignment, retry, unblock, and inspection;
- read-only shared workspaces and owned worktrees/branches for writers;
- router -> parallel workers -> verifier -> synthesizer patterns with explicit
  cost, fan-out, and depth limits.

"Kanban" should be a UI projection of this graph, not the core domain object.
The graph must reuse `RunRecord`, projects, permissions, evidence, and the normal
agent runtime instead of growing a second worker implementation.

## Then: code mode / programmatic tool composition

Let the model write a short program that can call a deliberately small proxy of
registered tools. This is an execution optimization inside one run, not a
workflow, goal, or privileged backdoor.

Requirements:

- memory, wall-time, output-size, and nested-call limits;
- cancellation and deterministic cleanup;
- no ambient filesystem, process, network, or credential access from the
  program itself;
- every nested call passes through the normal registry, permissions, capability
  policy, idempotency, and evidence path;
- only a bounded return value enters model context; intermediate values remain
  in the isolate unless explicitly returned;
- a narrow allowlist initially, with read-heavy composition before mutations.

Code mode should be usable by an ordinary session, goal turn, child run, or
work-graph worker without changing their lifecycle semantics.

## Then: workflows, skills, and learning surfaces

Skills are the reusable-procedure layer and should remain distinct from live
goals and work items. Strengthen the existing skill system with:

- version and provenance metadata;
- supporting references, scripts, templates, and declared capability needs;
- the skill versions actually loaded recorded on each run;
- validation and reviewable patch operations;
- outcome/evidence links so a host can evaluate a revision;
- rollback and protection for user-owned or pinned skills.

code-assistant should support explicit project-local learning and reusable
coding playbooks. PAL remains responsible for ambient personal learning,
cross-channel observation, dreaming schedules, aging, and consent policy. A
shared memory/retrieval provider interface may emerge, but the two products do
not need identical memory retention semantics.

## Later: durable approvals and execution targets

Generalize the existing permission mediator into two related paths:

- synchronous permission prompts for a live interactive run;
- an optional durable approval record for a suspended run.

A durable approval binds the exact proposed consequence, policy snapshot,
expiry, preconditions, and idempotency key. Approval revalidates current policy
and preconditions before executing. The host supplies presentation and delivery:
GUI/ACP in code-assistant, channels and outbox in PAL.

Sequencing note: PAL ranks durable action intents P0 — they gate unattended
outward actions — and will build them in `pal_core` first. This section then
becomes a deliberate second migration under the extract-when-a-consumer-exists
rule; it is not a reason for PAL to wait for the upstream generalization.

Define execution targets independently of goals and workers:

- local sandbox;
- owned git worktree;
- disposable container;
- explicitly configured remote runner.

Targets advertise capabilities and health, receive only scoped secrets, stream
bounded progress, support cancellation, and return typed artifacts/evidence.
Remote placement must not weaken the originating permission policy.

## Product quality track

The orchestration roadmap does not replace the existing correctness work of the
coding agent. Continue to improve the core editing experience in parallel:

- reject stale `replace_in_file` operations before the model compounds them;
- compact failed/mismatched tool-use history safely;
- tighten sandboxing around project-owned and git-tracked files;
- improve fuzzy-but-safe edit matching without hiding conflicts;
- make context compaction reliable and preserve active goals, plans, skills,
  and project instructions;
- add recovery and fault-injection tests around session persistence, event-stream
  lag, cancellation, and cross-process locks.

## Architectural invariants

Future features should preserve these rules:

1. **One owner per concern.** Project owns context; Goal owns outcome; Plan owns
   a working outline; WorkGraph owns coordination; RunAttempt owns execution.
2. **One agent runtime.** Goals, jobs, graphs, and code mode dispatch through the
   same tool, permission, event, and evidence paths.
3. **Durability is explicit.** A fast in-memory child and a restart-safe child
   may share types without pretending to offer the same guarantee.
4. **Claims precede work.** Persistent owners record an in-flight attempt before
   dispatch so crashes cannot erase budget, ownership, or side-effect history.
5. **Completion requires evidence.** Narrative confidence is not proof.
6. **External effects are replay-aware.** Retried execution uses idempotency or
   stops for inspection when the prior outcome is unknown.
7. **Host policy stays outside domain state.** Channels, startup supervisors,
   config paths, and concrete stores enter through adapters.
8. **Simple work stays simple.** A one-turn request needs no Goal; a direct Goal
   needs no WorkGraph; an ordinary tool call needs no code mode.
9. **Authority never grows implicitly.** Skills, child agents, remote targets,
   code mode, and resumed work remain inside the originating capability envelope.
10. **Every background object is inspectable and cancellable.** There are no
    invisible autonomous loops that only exist in model context.

## Explicit non-goals for code-assistant

Unless a separate always-on host is deliberately introduced, code-assistant
does not need to duplicate PAL's:

- channel identity and message-delivery routing;
- automatic daily conversation rotation;
- cron/event-source service and durable outbox;
- unsolicited personal observation and cross-domain memory curation;
- NUC daemon health, startup reconciliation, and multi-day headless soak policy;
- promise to proactively resume every unfinished goal or child after process
  restart.

The reusable types and seams should make those guarantees possible. PAL is the
application that chooses to make them.
