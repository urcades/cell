# Plugin Events

Lifecycle hooks are live in the current Rust host.

## Events you can rely on today

- `pluginLoaded`
- `hostStartup`
- `sessionStarted`
- `sessionEnded`
- `promptStarted`
- `promptFinished`
- `commandStarted`
- `commandFinished`
- `toolStarted`
- `toolFinished`

## What each event means

- `pluginLoaded`: a plugin finished loading and entered the registry
- `hostStartup`: the host startup path completed and hook dispatch began
- `sessionStarted`: a new interactive or RPC-backed session started
- `sessionEnded`: a session ended cleanly
- `promptStarted`: a prompt entered active execution
- `promptFinished`: a prompt finished execution
- `commandStarted`: a plugin command began running
- `commandFinished`: a plugin command finished running
- `toolStarted`: a tool call began running
- `toolFinished`: a tool call finished running

## Hook context

Hook requests carry a structured context that can include:

- event name
- target plugin id
- workspace root
- session id
- provider id
- model id
- event-specific data payload

Not every field is present for every event.

## Event name versus hook name

The event name and the hook name are not the same thing.

- The event is the lifecycle key the host dispatches on, such as `sessionStarted`.
- The hook name is the plugin-defined label for one registration, such as `session-started`.

In the example plugin, the hook named `session-started` listens for the `sessionStarted` event.

## Execution model

- Hooks run synchronously.
- Hooks run in merged hook order.
- Hook failures are warnings.
- Timeouts are warnings.
- Malformed hook responses are warnings.
- The underlying app action still proceeds unless the core runtime itself fails.

## Ordering

Merged hooks are sorted by:

1. event name
2. priority, highest first
3. plugin id or name as the stable tie-break

Within a single event, that merged order is the dispatch order.

## Stop propagation

If a hook returns `stopPropagation`, the host stops calling later hooks for that same event.

That does not cancel the underlying app action. It only stops the remaining hooks for that event dispatch.

## Declared but not yet supported at runtime

These event names exist in the types but are not part of the supported runtime story yet:

- `pluginEnabled`
- `pluginDisabled`
- `providerRegistered`
- `modelRegistered`
- `hostShutdown`
