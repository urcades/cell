# Rust Pi Port Postmortem

## 1. Executive Judgment

This report reconstructs the Rust port from the current code layout, tests, and surviving project docs. It does **not** reconstruct a commit-by-commit migration history, because the nested Rust repository starts with fresh history at `990456d` and does not preserve the detailed pre-split lineage of the port.

The short answer is: **yes, the Rust implementation is now a faithful standalone product replica for normal use**. The core experience is there: interactive terminal use, session history and branching, model resolution, OAuth-backed providers, package and resource discovery, export, share, tools, and the Apple Terminal-focused surface that the parity work targeted.

The stronger claim is narrower: the Rust port is not a transliteration of the TypeScript codebase. It is a behavioral replica built through a different architecture. In several places that is an improvement. In several others it introduces new maintenance costs.

The strongest parts of the Rust port are its seam split and its product decomposition:

- TypeScript spreads product logic across `packages/ai`, `packages/coding-agent`, and `packages/tui`, with many large managers and interactive components.
- Rust turns the same product into narrower crates for session state, resources, packages, models, OAuth, tools, providers, TUI primitives, and CLI orchestration.

That makes the Rust workspace easier to reason about at the subsystem level than the TypeScript original. It also makes several behaviors more explicit and more testable.

The weakest parts are different:

- the interactive layer in `crates/pi-rust-cli/src/interactive.rs` has become a very large orchestration surface
- the TUI parity harness is powerful but tightly coupled to microcopy and layout
- the extension boundary is still only a contract, not a runtime host
- some settings and low-traffic controls are present earlier than their deeper Rust-native implementation

That last point matters for the stop rule. The broad parity program should stop being called “parity work” once the remaining list is limited to:

- a **Rust-native plugin host/runtime** as a separate future product track
- direct JS/TS extension execution and TypeScript-specific runtime UI injection as **inherent mismatches**
- narrow wording/layout cleanup that has no real product payoff

That stop point has effectively been reached. The one meaningful broad item that remains is not more terminal parity; it is a separate plugin/runtime roadmap.

Current verification note, as of this report:

- `cargo test --workspace` passes in `/Users/edouard/Developer/pi/rust`
- `env -u PI_TS_REPO cargo test -p pi-rust-cli --test tui_parity` is **not fully green today**; it fails one breadth-audit assertion for `share-missing-gh` in `crates/pi-rust-cli/tests/tui_parity.rs`

That does not change the architectural judgment above, but it does mean the current tree still has one verification loose end on the Rust-only parity suite.

## 2. Architecture Map

| TypeScript seam | Main TypeScript paths | Main Rust crates/paths | Classification | Judgment |
| --- | --- | --- | --- | --- |
| Provider streaming and model catalog | `packages/ai/src/stream.ts`, `packages/ai/src/providers/*`, `packages/ai/src/models.ts` | `crates/pi-rust-ai-providers/src/*`, `crates/pi-rust-models/src/lib.rs`, `crates/pi-rust-ai-core/src/*` | Architectural reinterpretation | Rust preserves the product contract, but rebuilds provider streaming and model resolution around `reqwest`, custom event mapping, and a Rust registry model. |
| Interactive app and TUI | `packages/coding-agent/src/modes/interactive/interactive-mode.ts`, `packages/tui/src/*` | `crates/pi-rust-cli/src/interactive.rs`, `crates/pi-rust-tui/src/*` | Architectural reinterpretation | The surface behavior is intentionally aligned, but the implementation is a fresh Rust-native terminal system rather than a library-for-library port. |
| Session runtime and agent loop | `packages/coding-agent/src/core/agent-core/*`, `packages/coding-agent/src/core/session-manager.ts` | `crates/pi-rust-core/src/lib.rs`, `crates/pi-rust-core/src/agent_session.rs`, `crates/pi-rust-session/src/manager.rs` | Behavior-preserving port | Rust kept the session JSONL model, branching/forking flows, and startup/runtime semantics close to the original product model. |
| Packages, resources, and settings | `packages/coding-agent/src/core/package-manager.ts`, `packages/coding-agent/src/core/resource-loader.ts`, `packages/coding-agent/src/core/settings-manager.ts` | `crates/pi-rust-packages/src/lib.rs`, `crates/pi-rust-resources/src/lib.rs`, `crates/pi-rust-config/src/*` | Architectural reinterpretation | Rust turns a more intertwined TypeScript runtime path into separate package, resource, and config layers. |
| OAuth and auth storage | `packages/ai/src/utils/oauth/*`, `packages/coding-agent/src/core/auth/*` | `crates/pi-rust-oauth/src/lib.rs` | Behavior-preserving port | Same product concern, rebuilt in Rust with local storage, refresh flows, and provider registration. |
| CLI entrypoint and export/share | `packages/coding-agent/src/main.ts`, `packages/coding-agent/src/core/export-html/*`, interactive share flows | `crates/pi-rust-cli/src/lib.rs`, `crates/pi-rust-core/src/export_html.rs`, `crates/pi-rust-cli/src/interactive.rs` | Deliberately divergent Rust-native design | The user-facing flow is similar, but the Rust exporter and share flow are separate implementations with no shared internal lineage. |
| Extensions and custom UI | `packages/coding-agent/src/core/extensions/*`, exported extension UI surface in `packages/coding-agent/src/index.ts` | `crates/pi-rust-plugins/src/lib.rs` | Deliberately divergent Rust-native design | Rust specifies a future plugin contract but does not carry over the TypeScript runtime host or JS execution model. |

## 3. What Feels Good

### Workspace seam split

The Rust workspace is structurally cleaner than the TypeScript monorepo for maintainers who want to understand the product by subsystem.

TypeScript centralizes a lot of product behavior in `packages/coding-agent`, while Rust turns equivalent seams into dedicated crates:

- `crates/pi-rust-core`
- `crates/pi-rust-session`
- `crates/pi-rust-resources`
- `crates/pi-rust-packages`
- `crates/pi-rust-models`
- `crates/pi-rust-oauth`
- `crates/pi-rust-tools`
- `crates/pi-rust-tui`

That split is not cosmetic. It creates clearer ownership boundaries around session persistence, resource discovery, package installation, model lookup, and terminal rendering than the TypeScript side currently has.

### Session, resource, package, and config decomposition

The Rust port improved observability around resource and package behavior.

In TypeScript, package install/update, resource discovery, extension loading, and context-file lookup meet in a more intertwined runtime path under `packages/coding-agent/src/core/package-manager.ts` and `packages/coding-agent/src/core/resource-loader.ts`.

In Rust:

- `crates/pi-rust-packages/src/lib.rs` owns package installation and listing
- `crates/pi-rust-resources/src/lib.rs` owns resource cataloging and discovery
- `crates/pi-rust-config/src/*` owns layered config state

That gives the Rust port a more inspectable pipeline. `ResourceCatalog`, `ResourceCatalogGroup`, and `ResourceCatalogEntry` in `crates/pi-rust-resources/src/lib.rs` are especially strong because they preserve more of the “why” behind discovery outcomes, not just the final answer.

### Provider, model, and auth separation

The provider/model/auth seams are cleaner in Rust than they are in the product’s TypeScript layering.

- TypeScript uses provider SDKs and product-specific helpers across `packages/ai/src/providers/*`, `packages/ai/src/stream.ts`, and the coding-agent runtime.
- Rust separates those concerns into `pi-rust-ai-providers`, `pi-rust-models`, and `pi-rust-oauth`.

`crates/pi-rust-models/src/lib.rs` and `crates/pi-rust-oauth/src/lib.rs` make it much easier to reason about model availability, auth source precedence, and provider registration without reading the whole interactive runtime.

### TUI testability and parity discipline

The Rust port took parity seriously enough to build a real capture-based harness instead of depending on impressionistic manual checks.

- TypeScript interactive behavior is spread across `packages/coding-agent/src/modes/interactive/interactive-mode.ts` and `packages/tui/src/*`
- Rust verifies the same surfaces with tmux-based captures in `crates/pi-rust-cli/tests/tui_parity.rs` and `scripts/tui_parity_runner.mjs`

That is one of the strongest signs that this port became a product port instead of a prototype.

## 4. What Feels Brittle

### The interactive layer is too large

`crates/pi-rust-cli/src/interactive.rs` now holds too much of the application:

- rendering rules
- key handling
- overlay behavior
- startup/help logic
- active-run state
- auth/share/export surfaces
- config/resource UI
- transcript formatting

It works, but it is architecture under stress. The current behavior is explicit and testable, but unrelated edits can still collide because one file has become the center of gravity.

### The parity harness is coupled to microcopy and layout

The parity suite is a strength, but it is also brittle. `crates/pi-rust-cli/tests/tui_parity.rs` is necessarily sensitive to:

- exact line order
- exact wording
- exact footer shape
- exact visibility of particular status rows

That is the right tool for finishing a terminal parity push. It is also expensive to maintain long term. The recent `share-missing-gh` failure is a good example: one specialized scenario can fail even when the broad product judgment remains sound.

### Transitional helpers and partially applied settings

The current tree still shows signs of parity-era layering:

- several unused helpers and dead-code warnings remain in `interactive.rs`
- some settings are persisted earlier than they are deeply exercised
- some surfaces were clearly built to satisfy parity targets first and architectural cleanup second

That is not unusual for a port, but it is exactly the sort of thing that becomes maintenance drag if left untouched.

### Rust config is simpler, but also less defensive

Rust’s settings/config path is lighter than the TypeScript side, which is good for clarity. It is not yet as robust around concurrent writes and product-shaped helper APIs as the TypeScript settings layer in `packages/coding-agent/src/core/settings-manager.ts`.

The Rust side is easier to understand; the TypeScript side is harder to break under mixed write scenarios.

### The model catalog will drift unless it gets a clearer source of truth

The model-resolution logic is close between the two implementations, but the model catalog size is not. The TypeScript side currently ships a generated model catalog in `packages/ai/src/models.generated.ts` with 316 built-in entries. The Rust side hardcodes seven built-in models in `crates/pi-rust-models/src/lib.rs` and relies on overrides for the rest.

That is workable for the current product target, but it is a maintenance risk. The longer-term danger is not that model resolution is wrong; it is that the catalog quietly stops tracking the upstream product unless someone defines a durable source-of-truth strategy.

## 5. What Is Disconnected From The Original

These are not “bugs.” They are real architectural distances from the TypeScript product.

### The plugin boundary

TypeScript has a real extension system under `packages/coding-agent/src/core/extensions/*`, along with exported extension-facing UI/runtime hooks in `packages/coding-agent/src/index.ts`.

Rust has a contract crate at `crates/pi-rust-plugins/src/lib.rs`, but it is intentionally declarative. It does not include:

- a runtime host
- dynamic loading
- JS/TS execution
- custom UI injection

That is a product boundary, not a parity oversight.

### The terminal implementation

TypeScript’s terminal/UI stack is a reusable framework in `packages/tui/src/*`. Rust rebuilt the terminal layer as a custom renderer and widget set in `crates/pi-rust-tui/src/*`.

That means the visible behavior is similar, but the internals are fundamentally unrelated.

### Export and share internals

The Rust exporter in `crates/pi-rust-core/src/export_html.rs` and the Rust share flow in `crates/pi-rust-cli/src/interactive.rs` are Rust-native implementations. They aim at similar user outcomes, but they do not preserve the same internal structure as the TypeScript export pipeline in `packages/coding-agent/src/core/export-html/*`.

### User-level environment shape

The Rust project intentionally keeps project config under `.pi`, but its global/user-level environment is not a drop-in match for the TypeScript monorepo layout. That is a deliberate decoupling choice, not an accidental drift.

## 6. How The Port Was Actually Done

### Interactive / TUI

**Conceptually mirrored**

- startup/help stack
- prompt editor behavior
- selector and settings surfaces
- footer/status behavior
- transcript, tool, bash, and diff surfaces
- Apple Terminal-targeted rendering expectations

TypeScript anchors:

- `packages/coding-agent/src/modes/interactive/interactive-mode.ts`
- `packages/tui/src/tui.ts`
- `packages/tui/src/components/*`

Rust anchors:

- `crates/pi-rust-cli/src/interactive.rs`
- `crates/pi-rust-tui/src/lib.rs`
- `crates/pi-rust-tui/src/widgets.rs`

**Rewritten from scratch**

- terminal renderer
- widget composition model
- editor/input internals
- large parts of transcript formatting and diff/border rendering

**Rust ecosystem equivalent**

- terminal primitives via the `crossterm` class of APIs and Rust unicode crates
- custom render logic instead of a full TUI framework replacement

**Hardest conceptual gap**

Streaming and “thinking” behavior. The TypeScript runtime already had an event-rich interactive loop, while the Rust port had to make streamed thinking, partial assistant output, loader placement, and Apple Terminal quirks line up in a fresh implementation.

### Core / runtime / session / resources / packages

**Conceptually mirrored**

- session JSONL model
- continue/fork/branch flows
- package installation and listing
- resource discovery and resource configuration
- startup/runtime resource loading

TypeScript anchors:

- `packages/coding-agent/src/core/agent-core/*`
- `packages/coding-agent/src/core/session-manager.ts`
- `packages/coding-agent/src/core/package-manager.ts`
- `packages/coding-agent/src/core/resource-loader.ts`

Rust anchors:

- `crates/pi-rust-core/src/lib.rs`
- `crates/pi-rust-core/src/agent_session.rs`
- `crates/pi-rust-session/src/manager.rs`
- `crates/pi-rust-packages/src/lib.rs`
- `crates/pi-rust-resources/src/lib.rs`

**Rewritten from scratch**

- crate boundaries
- package/resource pipeline
- resource catalog model
- a number of control-plane and writeback paths

**Rust ecosystem equivalent**

- filesystem walking and glob/pattern behavior via `walkdir` and `globset`
- structured config/manifest parsing via `serde`, `serde_json`, and `serde_yaml`

**Hardest conceptual gap**

Package and resource filtering semantics. Matching TypeScript’s include/exclude/writeback behavior while also making the Rust model explicit enough to render enabled and disabled state was one of the hardest cross-language translations in the whole port.

### Providers / models / auth

**Conceptually mirrored**

- provider registry
- model lookup and scoped model resolution
- OAuth login and refresh flows
- auth storage precedence

TypeScript anchors:

- `packages/ai/src/providers/*`
- `packages/ai/src/stream.ts`
- `packages/ai/src/models.ts`
- `packages/ai/src/utils/oauth/*`
- `packages/coding-agent/src/core/model-resolver.ts`

Rust anchors:

- `crates/pi-rust-ai-providers/src/openai.rs`
- `crates/pi-rust-ai-providers/src/anthropic.rs`
- `crates/pi-rust-models/src/lib.rs`
- `crates/pi-rust-oauth/src/lib.rs`

**Rewritten from scratch**

- provider stream parsing
- provider request construction
- OAuth storage and refresh plumbing

**Rust ecosystem equivalent**

- `reqwest` instead of provider SDK-heavy TS internals
- custom stream/event mapping instead of directly reusing the TypeScript event system

**Hardest conceptual gap**

Provider identity, auth source precedence, and subscription/account semantics. The TypeScript product already lived inside a JS runtime with its own SDK and auth assumptions. Rust had to rebuild that product behavior against different HTTP and storage primitives.

One additional maintenance gap is catalog scale. The TypeScript side can regenerate a large built-in catalog; the Rust side currently carries a much smaller built-in set and depends more on explicit overrides.

### Tools / export / share

**Conceptually mirrored**

- tool transcript surfaces
- export command
- share flow
- HTML session viewer

TypeScript anchors:

- `packages/coding-agent/src/core/export-html/index.ts`
- `packages/coding-agent/src/core/export-html/template.css`
- interactive share/export flows in `packages/coding-agent/src/modes/interactive/interactive-mode.ts`

Rust anchors:

- `crates/pi-rust-tools/src/*`
- `crates/pi-rust-core/src/export_html.rs`
- `crates/pi-rust-cli/src/lib.rs`
- `crates/pi-rust-cli/src/interactive.rs`

**Rewritten from scratch**

- export viewer generation
- share orchestration
- transcript renderers for many tool blocks

**Rust ecosystem equivalent**

- `similar` for diffing
- Rust string/template generation rather than a TypeScript HTML/template stack

**Hardest conceptual gap**

Getting the surfaces to feel equivalent while accepting that the internals would not be. Export/share parity ended up being more about outcome matching than about preserving architecture.

### Plugins / extensions

**Conceptually mirrored**

- recognition that the product needs a plugin/extension capability boundary
- registration concepts for tools, commands, resources, and models

TypeScript anchors:

- `packages/coding-agent/src/core/extensions/*`
- exports in `packages/coding-agent/src/index.ts`

Rust anchors:

- `crates/pi-rust-plugins/src/lib.rs`
- `docs/extension-capability-matrix.md`

**Rewritten from scratch**

- the entire boundary definition

**Rust ecosystem equivalent**

- none yet at runtime; this is a contract/spec layer, not a loaded plugin host

**Hardest conceptual gap**

Deciding what parity should mean. The port deliberately stopped short of “run JS/TS extensions inside Rust,” which is the correct boundary decision, but it also means plugin/runtime parity moved from “unfinished port” into “future Rust product track.”

## 7. Library Equivalence And Reimplementation Table

| Subsystem | TypeScript approach/library | Rust approach/library | Equivalent or bespoke | Why the Rust choice made sense |
| --- | --- | --- | --- | --- |
| Terminal I/O and rendering | Custom `@mariozechner/pi-tui` components in `packages/tui/src/*`, plus JS runtime terminal handling | `crates/pi-rust-tui/src/*`, custom renderer/widgets, unicode crates, terminal primitives | Bespoke rebuild | The product needed deterministic terminal behavior in Rust, not a framework-shaped mirror of the JS stack. |
| Streaming providers | TS provider files plus SDKs such as `openai`, `@anthropic-ai/sdk`, `undici` | `reqwest` plus custom event parsing in `pi-rust-ai-providers` | Partial equivalent | Rust kept the provider contract but chose native HTTP primitives and explicit mapping logic. |
| Model registry / resolver | `packages/ai/src/models.ts`, `packages/coding-agent/src/core/model-resolver.ts` | `crates/pi-rust-models/src/lib.rs` | Equivalent with reinterpretation | Same product job, cleaner standalone seam in Rust. |
| OAuth / auth storage | TS OAuth helpers and runtime auth integration | `crates/pi-rust-oauth/src/lib.rs` | Equivalent with bespoke implementation | Same user flows, different runtime/storage environment. |
| Ignore / glob / filtering | `glob`, `ignore`, `minimatch` | `walkdir`, `globset`, custom filtering logic | Equivalent with bespoke composition | Rust needed explicit control over discovery and writeback semantics. |
| Config / settings serialization | JS object model and product-specific settings manager | `serde`, `serde_json`, layered Rust config helpers | Equivalent with reinterpretation | Rust gets simpler structured config handling, but loses some higher-level product-shaped helper APIs. |
| Markdown / resource parsing | `marked`, frontmatter utilities | custom parsing plus `serde_yaml` and Rust-side resource loaders | Bespoke rebuild | The Rust port needed direct control over resource discovery and parsing. |
| Diffing / highlighting | `diff`, `cli-highlight` | `similar` plus Rust-side formatting | Partial equivalent | Smaller dependency surface, explicit output control for terminal parity. |
| Image handling | `file-type`, `photon-node`, TUI image support | narrower Rust-native handling; no exact runtime analogue | Deliberately narrower | Full JS-native media tooling was not required to hit the product target. |
| Export viewer generation | TypeScript export pipeline in `packages/coding-agent/src/core/export-html/*` | `crates/pi-rust-core/src/export_html.rs` | Bespoke rebuild | Output parity mattered more than sharing implementation shape. |

## 8. Largest Conceptual Gaps

### Streaming, thinking, and active transcript behavior

This was one of the hardest parity problems because the visual rules are easy to notice and hard to fake. The TypeScript interactive path already had a mature event structure for:

- partial assistant text
- thinking deltas
- tool lifecycle events
- active status lines

Rust had to rebuild that behavior from scratch in `crates/pi-rust-cli/src/interactive.rs` and validate it against tmux captures in `crates/pi-rust-cli/tests/tui_parity.rs`.

### Terminal renderer and Apple Terminal quirks

TypeScript’s terminal system was already a productized framework in `packages/tui/src/*`. Rust had to decide whether to look for a drop-in TUI framework or rebuild the needed behavior directly. It chose the latter, which was the right decision for parity control, but it created a larger custom surface to maintain.

### Session tree and branch model

The session tree itself translated well, but not cheaply. The TypeScript product already assumed:

- branch-aware session JSONL
- resume/fork/continue
- branch summaries and tree UI

Rust preserved those semantics through `SessionManager` and the agent session/runtime layers. That conservative choice was correct because session compatibility is a hard edge to redesign later.

### Package/resource filtering and writeback

This was a bigger conceptual gap than it first appeared. It was not enough to discover resources; Rust also had to:

- represent enabled and disabled state
- respect project-over-user precedence
- preserve TypeScript-like include/exclude semantics
- write changes back without guessing

That translation forced Rust to invent a clearer internal model than the TypeScript side exposes directly.

### OAuth/provider identity and subscription handling

The user-visible behavior is straightforward. The implementation is not. TypeScript relied on JS ecosystem libraries, existing runtime assumptions, and product-specific auth helpers. Rust rebuilt those flows against local auth storage and custom refresh logic in `crates/pi-rust-oauth/src/lib.rs`.

### Extension capability boundary

This is the cleanest example of “stop parity work here.” The TypeScript product treats runtime extensions, commands, tools, and custom UI surfaces as first-class. Rust now has a clear plugin contract, but no runtime host. That is not more TUI parity work. It is the next product track.

## 9. Recommendation And Next Steps

### Keep as-is

- the crate decomposition across session/resources/packages/models/auth/tools/TUI
- the JSONL session compatibility strategy
- the Rust-native provider/model/auth split
- the capture-based terminal parity discipline
- the explicit plugin boundary in `crates/pi-rust-plugins`

### Near-term cleanup worth doing

- break `crates/pi-rust-cli/src/interactive.rs` into narrower modules
- reduce dead-code and transitional helper buildup in the interactive layer
- clean up the remaining parity-suite loose end around `share-missing-gh`
- revisit config/settings write-safety where Rust is still thinner than TypeScript

### Separate future track

- Rust-native plugin host/runtime
- dynamic plugin discovery and execution
- runtime registration of tools/commands/providers/models through the plugin contract
- any control-plane or host loop work that exists only to make the plugin/runtime model concrete

This is the next product track. It should **not** be treated as unfinished parity.

### Inherent mismatch / do not chase

- direct JS/TS extension execution
- Node/Bun runtime compatibility inside the Rust product
- TypeScript-specific event-bus behaviors that depend on the JS runtime
- extension-injected custom UI as a parity requirement

Those are not realistic or desirable parity targets for a pure-Rust replica.

### Recommendation

Do not restart another broad parity program. Treat the parity effort as complete for normal standalone Rust use once the current `share-missing-gh` verification failure is cleaned up or explicitly reclassified.

After that, move to one of two things only:

1. maintenance cleanup inside the existing Rust product
2. a separate plugin/runtime roadmap

## 10. Appendix

### Representative module and function map

| Subsystem | TypeScript anchors | Rust anchors | Notes |
| --- | --- | --- | --- |
| Interactive entry and app shell | `packages/coding-agent/src/modes/interactive/interactive-mode.ts` | `crates/pi-rust-cli/src/interactive.rs` | Main orchestration surface in both worlds, but much larger as a single file in Rust. |
| TUI primitives and widgets | `packages/tui/src/tui.ts`, `packages/tui/src/components/*` | `crates/pi-rust-tui/src/lib.rs`, `crates/pi-rust-tui/src/widgets.rs`, `crates/pi-rust-tui/src/render.rs` | Rust rebuilt the widget layer rather than porting the TS component library directly. |
| CLI entry and non-interactive flows | `packages/coding-agent/src/main.ts` | `crates/pi-rust-cli/src/lib.rs` | Includes CLI export flow and top-level mode selection. |
| Agent session/runtime | `packages/coding-agent/src/core/agent-core/*` | `crates/pi-rust-core/src/lib.rs`, `crates/pi-rust-core/src/agent_session.rs` | `create_agent_session` is the main runtime entry point on the Rust side. |
| Session persistence and tree state | `packages/coding-agent/src/core/session-manager.ts` | `crates/pi-rust-session/src/manager.rs` | `SessionManager` is the key Rust type. |
| Packages and install/update flows | `packages/coding-agent/src/core/package-manager.ts` | `crates/pi-rust-packages/src/lib.rs` | Rust keeps installation/update concerns separate from resource discovery. |
| Resource discovery and catalog | `packages/coding-agent/src/core/resource-loader.ts` | `crates/pi-rust-resources/src/lib.rs` | `catalog_resources_with_options` and `ResourceCatalog` are the key Rust anchors. |
| Models and scoped model resolution | `packages/coding-agent/src/core/model-resolver.ts`, `packages/ai/src/models.ts` | `crates/pi-rust-models/src/lib.rs` | `resolve_model_scope` is the key Rust function. |
| Provider streaming | `packages/ai/src/providers/*`, `packages/ai/src/stream.ts` | `crates/pi-rust-ai-providers/src/openai.rs`, `crates/pi-rust-ai-providers/src/anthropic.rs` | Rust uses native HTTP and custom mapping logic. |
| OAuth/auth storage | `packages/ai/src/utils/oauth/*` | `crates/pi-rust-oauth/src/lib.rs` | `AuthStorage` and provider registration are central Rust anchors. |
| Export HTML | `packages/coding-agent/src/core/export-html/index.ts` | `crates/pi-rust-core/src/export_html.rs` | Rust exporter is fully owned by the Rust tree. |
| Tools | tool implementations under coding-agent runtime | `crates/pi-rust-tools/src/*` | Mostly Rust-native implementations aligned to product expectations. |
| Plugin boundary | `packages/coding-agent/src/core/extensions/*`, `packages/coding-agent/src/index.ts` | `crates/pi-rust-plugins/src/lib.rs` | Contract exists; runtime host does not. |

### Supporting docs in the Rust repo

- `docs/parity-residuals.md`
- `docs/extension-capability-matrix.md`
- `docs/repo-workflow.md`

These docs are still the right companions to this report:

- `parity-residuals.md` for the end-state parity classification
- `extension-capability-matrix.md` for the plugin/runtime boundary
- `repo-workflow.md` for the nested-repo operating model
