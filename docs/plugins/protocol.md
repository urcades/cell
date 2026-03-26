# Plugin Protocol

Rust plugin v1 uses a line-delimited JSON protocol over stdio.

## Flow

1. The host starts the plugin process.
2. The host sends a `HandshakeRequest`.
3. The plugin responds with a `Registration` message.
4. The host merges the manifest into its runtime registry.
5. The host sends command, tool, or hook requests as needed.
6. The plugin replies with the matching response type.
7. The host may finish with `ShutdownRequest`.

## Host messages

- `HandshakeRequest`
- `ShutdownRequest`
- `CommandRequest`
- `ToolRequest`
- `HookRequest`

## Plugin messages

- `Registration`
- `Log`
- `ShutdownAck`
- `CommandResponse`
- `CommandError`
- `ToolResponse`
- `ToolError`
- `HookResponse`
- `HookError`

## Response matching

Every request message carries a `requestId`.
The plugin must echo that same id in its response.

## Manifest validation

The host validates all of the following before it accepts a plugin:

- protocol version
- manifest version
- plugin id matches the descriptor id
- capability names are unique within the manifest

If any of those checks fail, the host rejects the plugin before it enters the live registry.
