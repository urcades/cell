# Plugin Capabilities

This page is the supported capability matrix for plugin authors.

## Live today

| Capability | Status | What it means |
| --- | --- | --- |
| Commands | Live | The host can invoke plugin commands and use them in command discovery surfaces. |
| Tools | Live | The host can invoke plugin tools during normal agent tool execution. |
| Hooks | Live | The host can dispatch supported lifecycle events to plugins. |

## Declared, but not part of the supported author story

| Capability | Status | What it means |
| --- | --- | --- |
| Flags | Declared only | Flag metadata can be registered, but there is no supported live flag flow yet. |
| Providers | Deferred | Registration is accepted and summarized, but provider execution is not live. |
| Models | Deferred | Registration is accepted and summarized, but model execution is not live. |

## Rules that matter in practice

- Capability names must be unique after merge
- Duplicate command, tool, provider, or model names are rejected
- Hidden commands stay out of the normal command surfaces
- Warnings are surfaced without crashing the app

## Current product boundary

If you want a plugin that actually runs today, build around:

- commands
- tools
- hooks

If you want provider or model execution, that is future work and should not be treated as a currently supported path.
