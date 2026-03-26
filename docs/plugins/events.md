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

## Execution model

- Hooks run synchronously in merged hook order
- Hooks are best-effort
- Hook warnings do not crash the app
- A timeout or malformed response becomes a warning

## Ordering

Merged hooks are sorted by:

1. event name
2. priority, highest first
3. plugin id/name as the stable tie-break

Within one event, that merged order is the order the host dispatches.

## Stop propagation

If a hook returns `stopPropagation`, the host stops calling later hooks for that same event.

That does not cancel the underlying app action. It only stops the remaining hooks for that event dispatch.

## Deferred events

These hook names exist in types but are not part of the supported runtime story yet:

- `pluginEnabled`
- `pluginDisabled`
- `providerRegistered`
- `modelRegistered`
- `hostShutdown`
