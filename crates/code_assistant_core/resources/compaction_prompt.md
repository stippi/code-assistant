<system-compaction>
You are performing a context checkpoint compaction: the conversation is nearing the model's context window limit. Write a hand-off for another instance of this assistant that will resume the task without access to the messages above.

Include:
- What the user asked for and what kind of response they expect (an answer, an explanation, a change to the workspace).
- Current progress: what has been done, what was decided and why.
- Verified facts the next instance would otherwise have to rediscover: relevant files with their paths and what they contain, commands run and their results, constraints and user preferences.
- What remains to be done, as concrete next steps.
- Open questions the user still has to answer.

Be structured and specific. The user's own messages are handed over verbatim alongside this text, so do not repeat them.
Do not call any tools in this response. Respond with plain text only.
</system-compaction>
