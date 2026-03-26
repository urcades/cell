# Plugin Protocol

The Rust plugin system uses a line-delimited JSON protocol over stdio.

## Transport rules

- The host writes one JSON message per line to the plugin's stdin.
- The plugin writes one JSON message per line to stdout.
- The plugin may write logs to stderr.
- Every runtime request carries a `requestId`.
- The plugin must echo the same `requestId` in its matching response.

## Startup flow

1. The host launches the plugin process.
2. The host sends `handshake_request`.
3. The plugin replies with `registration`.
4. The host validates the protocol version, manifest version, plugin identity, and capability names.
5. If validation succeeds, the plugin enters the live registry.

## Minimal handshake example

Host:

```json
{
  "type": "handshake_request",
  "protocolVersion": 1,
  "host": {
    "name": "cell",
    "version": "0.52.12"
  },
  "workspaceRoot": "/workspace"
}
```

Plugin:

```json
{
  "type": "registration",
  "protocolVersion": 1,
  "manifest": {
    "manifestVersion": 1,
    "plugin": {
      "id": "hello-plugin",
      "name": "Hello Plugin",
      "version": "0.1.0"
    },
    "commands": [],
    "tools": [],
    "flags": [],
    "hooks": []
  }
}
```

## Host messages

- `handshake_request`
- `shutdown_request`
- `command_request`
- `tool_request`
- `hook_request`

## Plugin messages

- `registration`
- `log`
- `shutdown_ack`
- `command_response`
- `command_error`
- `tool_response`
- `tool_error`
- `hook_response`
- `hook_error`

## Message ownership

The current wire types live in:

- `crates/cell-plugin-protocol`
- `crates/cell-plugins`

The host side is implemented in:

- `crates/cell-plugin-host`

## Validation rules

The host rejects a plugin before it enters the registry if any of these checks fail:

- unsupported protocol version
- unsupported manifest version
- descriptor id does not match manifest plugin id
- duplicate capability names inside the manifest
- duplicate capability names after host-wide merge

## Runtime request shapes

### Command requests

The host sends:

- `requestId`
- `commandName`
- `args`
- `cwd`
- optional `sessionId`
- optional `rawInput`

The plugin replies with either:

- `command_response` and a replacement string
- `command_error` and an error message

Example:

```json
{
  "type": "command_request",
  "requestId": "cmd-1",
  "commandName": "hello",
  "args": ["Ada", "Lovelace"],
  "cwd": "/workspace",
  "sessionId": "session-1",
  "rawInput": "hello Ada Lovelace"
}
```

```json
{
  "type": "command_response",
  "requestId": "cmd-1",
  "replacement": "hello:Ada|Lovelace"
}
```

### Tool requests

The host sends:

- `requestId`
- `toolCallId`
- `toolName`
- structured `arguments`
- `cwd`
- optional `sessionId`

The plugin replies with either:

- `tool_response` and structured content blocks
- `tool_error` and an error message

Example:

```json
{
  "type": "tool_request",
  "requestId": "tool-1",
  "toolCallId": "tool-call-1",
  "toolName": "echo",
  "arguments": {
    "text": "Ada"
  },
  "cwd": "/workspace",
  "sessionId": "session-1"
}
```

```json
{
  "type": "tool_response",
  "requestId": "tool-1",
  "content": [
    {
      "type": "text",
      "text": "tool:Ada"
    }
  ],
  "details": {
    "echo": "Ada"
  },
  "isError": false
}
```

### Hook requests

The host sends:

- `requestId`
- `hookName`
- a structured lifecycle context

The plugin replies with either:

- `hook_response` and a lifecycle outcome
- `hook_error` and an error message

Example:

```json
{
  "type": "hook_request",
  "requestId": "hook-1",
  "hookName": "session-started",
  "context": {
    "event": "sessionStarted",
    "pluginId": "subject-plugin",
    "workspaceRoot": "/workspace",
    "sessionId": "session-1",
    "data": {}
  }
}
```

```json
{
  "type": "hook_response",
  "requestId": "hook-1",
  "outcome": "continue"
}
```

## Shutdown

The host may finish with `shutdown_request`.
The plugin should answer with `shutdown_ack` and exit cleanly.
