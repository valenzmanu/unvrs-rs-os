---
name: frontier-efficient-work
description: >-
  Highest-quality work from GPT-6 Astra or Claude Fable 5.1 at minimum quota.
  Use when the session is Astra or Fable, or the user says frontier-efficient-work.
---

# frontier-efficient-work

You are Astra or Fable. The user does not pick effort, child models, or config. You do. On any other model, ignore this skill and do not change their default model.

## Config

Install only what is missing. Do not ask. Do not overwrite an existing value. Do not make Astra or Fable the default for every session. If you wrote nothing, say nothing.

Astra: in `~/.codex/config.toml`, set `[agents] default_subagent_model` to `gpt-5.6-terra` and `default_subagent_reasoning_effort` to `medium` only if those keys are unset. Copy any missing file from `codex/` next to this skill into `~/.codex/agents/`.

Fable: copy any missing file from `claude/` next to this skill into `~/.claude/agents/`. If `~/.claude/settings.json` has no `env.ANTHROPIC_DEFAULT_FABLE_MODEL`, set it to `claude-fable-5-1`. Do not set `CLAUDE_CODE_SUBAGENT_MODEL` or `availableModels`. Do not replace the built-in Explore agent.

## Work

You decide each step. Do it yourself when it is the reading of the task, the approach, an ambiguous decision, integration, or the final check against done. Do not delegate that check or a destructive action.

Otherwise name a child. Give it the files, the done check, and an order to return a summary without widening the task. Do not send this conversation. An unnamed child is another copy of you. You accept or reject what comes back.

| | Scan | Edit | Review |
| --- | --- | --- | --- |
| Astra | explorer · `gpt-5.6-luna` | worker · `gpt-5.6-terra` | reviewer · `gpt-5.6-sol`, only if the edit needs it |
| Fable | scanner · Haiku. Code search: built-in Explore | worker · Sonnet | reviewer · Opus, only if Sonnet should not own it |

## Scope and effort

Implement every behavior the task asks for, then stop. No extra feature, refactor, helper, file, future-proofing, handling for a case that cannot happen, comments on untouched code, or a permanent test this repo does not already keep for this kind of change. A nearby bug is a follow-up unless the requested behavior cannot work without it. An ambiguity gets the one reading the wording and nearby code support. A one-path change gets no planning pass.

Astra: `low` or `medium` effort. Finish the requested outcome, including the run and the fixes that run shows. Do not read the whole repo first, and do not ask permission for a safe local check. No `xhigh`, `max`, Ultra, or Fast. Keep your thread under 272K input tokens.

Fable: `high` effort. Lower a routine follow-up only when that kind of step has already held at `medium` or `low`. At `low`, search before answering a current fact. No `xhigh`, `max`, or Ultracode. Edit a span. Do not fork this conversation.
