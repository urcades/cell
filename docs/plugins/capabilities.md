# Plugin Capabilities

This page describes the supported capability classes from the point of view of a plugin author.

## Commands

Commands are live.

A command plugin can:

- register a command name
- describe parameters
- stay hidden or visible
- receive a `command_request`
- reply with replacement text or an error

Practical effect:

- plugin commands appear in command discovery surfaces
- plugin commands can be invoked by the host
- built-in command names still win when names collide

## Tools

Tools are live.

A tool plugin can:

- register a tool name
- define structured parameters
- declare an output kind
- receive a `tool_request`
- reply with structured content blocks or an error

Practical effect:

- plugin tools participate in the normal tool execution path
- plugin tool failures become tool errors or warnings, not host crashes

## Hooks

Hooks are live.

A hook plugin can:

- register one or more lifecycle events
- set a priority
- receive a `hook_request`
- reply with `continue` or `stopPropagation`

Practical effect:

- hooks run synchronously in merged hook order
- hook warnings do not crash the app
- `stopPropagation` only stops later hooks for that same event

## Flags

Flags are declared but not yet a supported live author surface.

Today they are:

- accepted in the manifest
- visible in diagnostics and summaries
- not yet bound into a live runtime flag path

## Providers and models

Provider and model registrations are accepted and summarized, but not executed.

That means:

- the host can validate and report them
- the current runtime does not route real provider or model execution through plugins

## Rules that matter in practice

- Capability names must stay unique after merge
- Duplicate command, tool, provider, or model names are rejected
- Hidden commands and tools stay out of normal user-facing discovery
- Warnings are surfaced without crashing the app

## Supported author target today

If you want a plugin that runs today, build around:

- commands
- tools
- hooks
