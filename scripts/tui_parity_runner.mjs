#!/usr/bin/env node

import { chmodSync, existsSync, mkdtempSync, mkdirSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { createServer } from "node:http";
import { randomUUID } from "node:crypto";
import { once } from "node:events";

const __filename = fileURLToPath(import.meta.url);
const SCRIPTS_DIR = dirname(__filename);
const RUST_DIR = dirname(SCRIPTS_DIR);
const DEFAULT_RUST_BIN = join(RUST_DIR, "target", "debug", "cell");

function resolveTsRepoDir() {
	const tsRepo = process.env.PI_TS_REPO?.trim();
	if (!tsRepo) {
		return null;
	}
	const resolved = resolve(tsRepo);
	if (!existsSync(resolved)) {
		throw new Error(`PI_TS_REPO does not exist: ${resolved}`);
	}
	return resolved;
}

function requireTsRepoDir() {
	const tsRepo = resolveTsRepoDir();
	if (!tsRepo) {
		throw new Error(
			"PI_TS_REPO is required for TS parity captures. Set it to the TypeScript repo root.",
		);
	}
	return tsRepo;
}

function tsxCliPath() {
	return join(requireTsRepoDir(), "node_modules", "tsx", "dist", "cli.mjs");
}

function tsCliPath() {
	return join(requireTsRepoDir(), "packages", "coding-agent", "src", "cli.ts");
}

function runtimeRootFor(runtime) {
	return runtime === "ts" ? requireTsRepoDir() : RUST_DIR;
}
const DEFAULT_SCENARIOS = [
	"startup",
	"manual-bash",
	"shift-tab",
	"settings",
	"resume",
	"startup-resume",
	"fork",
	"model",
	"scoped-models",
	"read",
	"write",
	"edit",
	"diff",
	"slash",
	"sticky-footer",
	"bash",
];
const EXTRA_SCENARIOS = [
	"config-browser",
	"startup-diagnostics",
	"resume-populated-management",
	"tree-navigation",
	"tree-summary",
	"tree-management",
	"fork-populated",
	"login",
	"login-populated",
	"logout",
	"logout-populated",
	"reload-diagnostics",
	"session",
	"changelog",
	"hotkeys",
	"builtins-results",
	"copy-empty",
	"share-missing-gh",
	"share-success",
	"share-cancel",
	"footer-variants",
	"footer-subscription",
	"footer-unknown-context",
	"excluded-bash",
	"hidden-thinking",
	"live-streaming-start",
	"live-streaming-mid",
	"abort-active-run",
	"custom-messages",
	"skill-messages",
	"tool-lifecycle",
	"custom-messages-and-skills",
	"compaction-and-retry",
];
const SUPPORTED_SCENARIOS = [...DEFAULT_SCENARIOS, ...EXTRA_SCENARIOS];
const SESSION_SCENARIOS = new Set([
	"read",
	"write",
	"edit",
	"tree-navigation",
	"tree-summary",
	"tree-management",
	"fork-populated",
	"login-populated",
	"logout-populated",
	"builtins-results",
	"footer-variants",
	"footer-subscription",
	"footer-unknown-context",
	"custom-messages",
	"skill-messages",
	"tool-lifecycle",
	"custom-messages-and-skills",
	"compaction-and-retry",
]);
const SEEDED_TOOL_SCENARIOS = new Set(["read", "write", "edit", "tool-lifecycle"]);
const API_ENV_VARS = [
	"ANTHROPIC_API_KEY",
	"ANTHROPIC_OAUTH_TOKEN",
	"OPENAI_API_KEY",
	"GEMINI_API_KEY",
	"GROQ_API_KEY",
	"CEREBRAS_API_KEY",
	"XAI_API_KEY",
	"OPENROUTER_API_KEY",
	"ZAI_API_KEY",
	"MISTRAL_API_KEY",
	"MINIMAX_API_KEY",
	"MINIMAX_CN_API_KEY",
	"AI_GATEWAY_API_KEY",
	"OPENCODE_API_KEY",
	"COPILOT_GITHUB_TOKEN",
	"GH_TOKEN",
	"GITHUB_TOKEN",
	"GOOGLE_APPLICATION_CREDENTIALS",
	"GOOGLE_CLOUD_PROJECT",
	"GCLOUD_PROJECT",
	"GOOGLE_CLOUD_LOCATION",
	"AWS_PROFILE",
	"AWS_ACCESS_KEY_ID",
	"AWS_SECRET_ACCESS_KEY",
	"AWS_SESSION_TOKEN",
	"AWS_REGION",
	"AWS_DEFAULT_REGION",
	"AWS_BEARER_TOKEN_BEDROCK",
	"AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
	"AWS_CONTAINER_CREDENTIALS_FULL_URI",
	"AWS_WEB_IDENTITY_TOKEN_FILE",
	"AZURE_OPENAI_API_KEY",
	"AZURE_OPENAI_BASE_URL",
	"AZURE_OPENAI_RESOURCE_NAME",
];

const LIVE_RESPONSE_SCENARIOS = new Set([
	"live-streaming-start",
	"live-streaming-mid",
	"abort-active-run",
	"hidden-thinking",
	"tool-lifecycle",
]);

function isLiveResponseScenario(scenario) {
	return LIVE_RESPONSE_SCENARIOS.has(scenario);
}

function scenarioRuntimeReadyText(runtime, scenario) {
	switch (`${runtime}:${scenario}`) {
		case "ts:config-browser":
		case "rust:config-browser":
			return ["Resource Configuration", "Type to filter resources"];
		case "ts:footer-variants":
		case "rust:footer-variants":
			return ["Paritied Session", "gpt-4.1"];
		case "ts:footer-subscription":
		case "rust:footer-subscription":
			return ["Paritied Session", "gpt-5.3-codex"];
		case "ts:footer-unknown-context":
		case "rust:footer-unknown-context":
			return ["Paritied Session", "?/0"];
		case "ts:live-streaming-start":
		case "rust:live-streaming-start":
			return ["Live streaming start", "Waiting for model response..."];
		case "ts:live-streaming-mid":
		case "rust:live-streaming-mid":
			return ["Live streaming mid", "Waiting for model response..."];
		case "ts:abort-active-run":
		case "rust:abort-active-run":
			return ["Request aborted", "Operation aborted"];
		case "ts:hidden-thinking":
		case "rust:hidden-thinking":
			return ["Thinking...", "Hidden thinking response"];
		case "ts:tool-lifecycle":
		case "rust:tool-lifecycle":
			return ["Bash", "tool-lifecycle complete"];
		default:
			return [];
	}
}

function scenarioLauncherArgs(scenario, sessionPath, extraArgs) {
	if (scenario === "config-browser") {
		return ["config"];
	}
	if (scenario === "footer-subscription" || scenario === "footer-unknown-context") {
		return [
			...(sessionPath ? ["--session", sessionPath] : ["--no-session"]),
			"--provider",
			"openai-codex",
			"--model",
			"gpt-5.3-codex",
			"--no-tools",
			"--thinking",
			"off",
			...(extraArgs || []),
		];
	}

	const thinkingArgs =
		scenario === "hidden-thinking" ||
		scenario === "live-streaming-start" ||
		scenario === "live-streaming-mid"
			? ["--thinking", "minimal"]
			: ["--thinking", "off"];

	const baseArgs =
		scenario === "startup-resume"
			? ["--resume"]
			: [
					...(sessionPath ? ["--session", sessionPath] : ["--no-session"]),
					"--provider",
					"openai",
					"--model",
					"gpt-4.1",
					...(scenario === "tool-lifecycle" ? [] : ["--no-tools"]),
					...thinkingArgs,
				];

	return [...baseArgs, ...(extraArgs || [])];
}

function scenarioPromptText(scenario) {
	switch (scenario) {
		case "live-streaming-start":
			return "Show the stream starting.";
		case "live-streaming-mid":
			return "Show the stream mid-flight.";
		case "abort-active-run":
			return "Start a run we can abort.";
		case "hidden-thinking":
			return "Reveal hidden reasoning.";
		case "tool-lifecycle":
			return "Run the tool lifecycle.";
		default:
			return "Hello from parity.";
	}
}

function writeModelsFixture(homeDir, baseUrl) {
	writeModelsJsonFixture(homeDir, {
		providers: {
			openai: {
				baseUrl,
			},
		},
	});
}

function writeModelsJsonFixture(homeDir, config) {
	const agentDir = join(homeDir, ".pi", "agent");
	ensureDir(agentDir);
	writeFileSync(join(agentDir, "models.json"), JSON.stringify(config, null, 2), "utf8");
}

function writeUnknownContextModelsFixture(homeDir) {
	writeModelsJsonFixture(homeDir, {
		providers: {
			"openai-codex": {
				modelOverrides: {
					"gpt-5.3-codex": {
						contextWindow: 0,
					},
				},
			},
		},
	});
}

function writeGhFixture(tempRoot, mode) {
	const binDir = join(tempRoot, "bin");
	ensureDir(binDir);
	const ghPath = join(binDir, "gh");
	const script =
		mode === "success"
			? `#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == "auth" && "$2" == "status" ]]; then
  exit 0
fi
if [[ "$1" == "gist" && "$2" == "create" ]]; then
  printf '%s\n' 'https://gist.github.com/cell/fake-share-id'
  exit 0
fi
echo "unexpected gh invocation" >&2
exit 1
`
			: `#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == "auth" && "$2" == "status" ]]; then
  exit 0
fi
if [[ "$1" == "gist" && "$2" == "create" ]]; then
  sleep 30
  exit 0
fi
echo "unexpected gh invocation" >&2
exit 1
`;
	writeFileSync(ghPath, script, "utf8");
	chmodSync(ghPath, 0o755);
	return binDir;
}

function writeSettingsFixture(homeDir, patch) {
	const agentDir = join(homeDir, ".pi", "agent");
	ensureDir(agentDir);
	writeFileSync(join(agentDir, "settings.json"), JSON.stringify(patch, null, 2), "utf8");
}

function createSseEventWriter(response) {
	return (event) => {
		response.write(`data: ${JSON.stringify(event)}\n\n`);
	};
}

function delay(ms) {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

function assistantContentBlockText(text, textSignature = null) {
	return {
		type: "text",
		text,
		textSignature,
	};
}

function assistantContentBlockThinking(thinking, thinkingSignature = null) {
	return {
		type: "thinking",
		thinking,
		thinkingSignature,
	};
}

function responseFunctionCallItem(callId, itemId, name, argumentsText) {
	return {
		type: "function_call",
		call_id: callId,
		id: itemId,
		name,
		arguments: argumentsText,
	};
}

function responseMessageItem(id, text = "", status = "in_progress") {
	return {
		type: "message",
		id,
		role: "assistant",
		status,
		content: [
			{
				type: "output_text",
				text,
				annotations: [],
			},
		],
	};
}

function responseReasoningItem(id, summaryText = "", status = "in_progress") {
	return {
		type: "reasoning",
		id,
		status,
		summary: summaryText
			? [
					{
						type: "summary_text",
						text: summaryText,
					},
				]
			: [],
	};
}

function buildResponsesStreamPlan(scenario, requestIndex) {
	switch (scenario) {
		case "live-streaming-start":
			return {
				async run(writeEvent, response) {
					writeEvent({
						type: "response.output_item.added",
						item: responseReasoningItem("reasoning_live_start"),
					});
					writeEvent({
						type: "response.reasoning_summary_part.added",
						item_id: "reasoning_live_start",
						output_index: 0,
						part: {
							type: "summary_text",
							text: "",
						},
						sequence_number: 1,
						summary_index: 0,
					});
					await delay(120);
					writeEvent({
						type: "response.reasoning_summary_text.delta",
						delta: "Streaming visible thinking start",
					});
					await delay(120);
					writeEvent({
						type: "response.reasoning_summary_part.done",
						item_id: "reasoning_live_start",
						output_index: 0,
						part: {
							type: "summary_text",
							text: "Streaming visible thinking start",
						},
						sequence_number: 2,
						summary_index: 0,
					});
					writeEvent({
						type: "response.output_item.done",
						item: responseReasoningItem(
							"reasoning_live_start",
							"Streaming visible thinking start",
							"completed",
						),
					});
					writeEvent({
						type: "response.output_item.added",
						item: responseMessageItem("msg_live_start"),
					});
					await delay(120);
					writeEvent({
						type: "response.output_text.delta",
						delta: "Live streaming ",
					});
					await delay(900);
					writeEvent({
						type: "response.output_text.delta",
						delta: "start",
					});
					await delay(1500);
					writeEvent({
						type: "response.output_item.done",
						item: responseMessageItem("msg_live_start", "Live streaming start", "completed"),
					});
					writeEvent({
						type: "response.completed",
						response: {
							status: "completed",
							usage: {
								input_tokens: 10,
								output_tokens: 4,
								input_tokens_details: { cached_tokens: 0 },
							},
						},
					});
					response.write("data: [DONE]\n\n");
					response.end();
				},
			};
		case "live-streaming-mid":
			return {
				async run(writeEvent, response) {
					writeEvent({
						type: "response.output_item.added",
						item: responseReasoningItem("reasoning_live_mid"),
					});
					writeEvent({
						type: "response.reasoning_summary_part.added",
						item_id: "reasoning_live_mid",
						output_index: 0,
						part: {
							type: "summary_text",
							text: "",
						},
						sequence_number: 1,
						summary_index: 0,
					});
					await delay(120);
					writeEvent({
						type: "response.reasoning_summary_text.delta",
						delta: "Streaming visible thinking ",
					});
					await delay(350);
					writeEvent({
						type: "response.reasoning_summary_text.delta",
						delta: "mid",
					});
					await delay(120);
					writeEvent({
						type: "response.reasoning_summary_part.done",
						item_id: "reasoning_live_mid",
						output_index: 0,
						part: {
							type: "summary_text",
							text: "Streaming visible thinking mid",
						},
						sequence_number: 2,
						summary_index: 0,
					});
					writeEvent({
						type: "response.output_item.done",
						item: responseReasoningItem(
							"reasoning_live_mid",
							"Streaming visible thinking mid",
							"completed",
						),
					});
					writeEvent({
						type: "response.output_item.added",
						item: responseMessageItem("msg_live_mid"),
					});
					await delay(120);
					writeEvent({
						type: "response.output_text.delta",
						delta: "Live ",
					});
					await delay(450);
					writeEvent({
						type: "response.output_text.delta",
						delta: "streaming ",
					});
					await delay(900);
					writeEvent({
						type: "response.output_text.delta",
						delta: "mid",
					});
					await delay(1500);
					writeEvent({
						type: "response.output_item.done",
						item: responseMessageItem("msg_live_mid", "Live streaming mid", "completed"),
					});
					writeEvent({
						type: "response.completed",
						response: {
							status: "completed",
							usage: {
								input_tokens: 10,
								output_tokens: 6,
								input_tokens_details: { cached_tokens: 0 },
							},
						},
					});
					response.write("data: [DONE]\n\n");
					response.end();
				},
			};
		case "abort-active-run":
			return {
				async run(writeEvent, response) {
					writeEvent({
						type: "response.output_item.added",
						item: responseMessageItem("msg_abort"),
					});
					await delay(150);
					writeEvent({
						type: "response.output_text.delta",
						delta: "Aborting live run",
					});
					await delay(5000);
					writeEvent({
						type: "response.output_item.done",
						item: responseMessageItem("msg_abort", "Aborting live run", "completed"),
					});
					writeEvent({
						type: "response.completed",
						response: {
							status: "completed",
							usage: {
								input_tokens: 10,
								output_tokens: 4,
								input_tokens_details: { cached_tokens: 0 },
							},
						},
					});
					response.write("data: [DONE]\n\n");
					response.end();
				},
			};
		case "hidden-thinking":
			return {
				async run(writeEvent, response) {
					writeEvent({
						type: "response.output_item.added",
						item: responseReasoningItem("reasoning_hidden"),
					});
					writeEvent({
						type: "response.reasoning_summary_part.added",
						item_id: "reasoning_hidden",
						output_index: 0,
						part: {
							type: "summary_text",
							text: "",
						},
						sequence_number: 1,
						summary_index: 0,
					});
					await delay(120);
					writeEvent({
						type: "response.reasoning_summary_text.delta",
						delta: "Hidden reasoning block",
					});
					await delay(120);
					writeEvent({
						type: "response.reasoning_summary_part.done",
						item_id: "reasoning_hidden",
						output_index: 0,
						part: {
							type: "summary_text",
							text: "Hidden reasoning block",
						},
						sequence_number: 2,
						summary_index: 0,
					});
					writeEvent({
						type: "response.output_item.done",
						item: responseReasoningItem("reasoning_hidden", "Hidden reasoning block", "completed"),
					});
					writeEvent({
						type: "response.output_item.added",
						item: responseMessageItem("msg_hidden"),
					});
					await delay(120);
					writeEvent({
						type: "response.output_text.delta",
						delta: "Hidden thinking response",
					});
					await delay(300);
					writeEvent({
						type: "response.output_item.done",
						item: responseMessageItem("msg_hidden", "Hidden thinking response", "completed"),
					});
					writeEvent({
						type: "response.completed",
						response: {
							status: "completed",
							usage: {
								input_tokens: 10,
								output_tokens: 5,
								input_tokens_details: { cached_tokens: 0 },
							},
						},
					});
					response.write("data: [DONE]\n\n");
					response.end();
				},
			};
		case "tool-lifecycle":
			if (requestIndex === 0) {
					return {
						async run(writeEvent, response) {
							writeEvent({
								type: "response.output_item.added",
								item: responseFunctionCallItem(
									"call_tool_lifecycle",
									"tool_call_1",
									"bash",
									"{\"command\":\"printf tool-lifecycle\"}",
								),
							});
							await delay(100);
							writeEvent({
								type: "response.function_call_arguments.delta",
								delta: "{\"command\":\"printf tool-lifecycle\"}",
							});
							await delay(120);
							writeEvent({
								type: "response.function_call_arguments.done",
								arguments: "{\"command\":\"printf tool-lifecycle\"}",
							});
							writeEvent({
								type: "response.output_item.done",
								item: responseFunctionCallItem(
									"call_tool_lifecycle",
									"tool_call_1",
									"bash",
									"{\"command\":\"printf tool-lifecycle\"}",
								),
							});
							writeEvent({
								type: "response.completed",
								response: {
									status: "completed",
									usage: {
										input_tokens: 10,
										output_tokens: 2,
										input_tokens_details: { cached_tokens: 0 },
									},
								},
							});
							response.write("data: [DONE]\n\n");
							response.end();
					},
				};
			}
			return {
				async run(writeEvent, response) {
					writeEvent({
						type: "response.output_item.added",
						item: responseMessageItem("msg_tool_done"),
					});
					await delay(100);
					writeEvent({
						type: "response.output_text.delta",
						delta: "tool-lifecycle complete",
					});
					await delay(100);
					writeEvent({
						type: "response.output_item.done",
						item: responseMessageItem("msg_tool_done", "tool-lifecycle complete", "completed"),
					});
					writeEvent({
						type: "response.completed",
						response: {
							status: "completed",
							usage: {
								input_tokens: 12,
								output_tokens: 3,
								input_tokens_details: { cached_tokens: 0 },
							},
						},
					});
					response.write("data: [DONE]\n\n");
					response.end();
				},
			};
		default:
			return null;
	}
}

async function startResponsesServer(scenario) {
	let requestIndex = 0;
	const server = createServer((req, res) => {
		if (req.method !== "POST" || !req.url?.includes("/responses")) {
			res.writeHead(404, { "content-type": "text/plain; charset=utf-8" });
			res.end("not found");
			return;
		}

		let requestBody = "";
		req.setEncoding("utf8");
		req.on("data", (chunk) => {
			requestBody += chunk;
		});
		req.on("end", async () => {
			void requestBody;
			const plan = buildResponsesStreamPlan(scenario, requestIndex);
			requestIndex += 1;
			res.writeHead(200, {
				"content-type": "text/event-stream; charset=utf-8",
				"cache-control": "no-cache, no-transform",
				connection: "keep-alive",
			});
			const writeEvent = createSseEventWriter(res);
			if (plan) {
				try {
					await plan.run(writeEvent, res);
				} catch (error) {
					const message = error instanceof Error ? error.message : String(error);
					try {
						res.write(
							`data: ${JSON.stringify({
								type: "response.failed",
								response: { error: { message } },
							})}\n\n`,
						);
					} catch {
						// Ignore write failures when the client has already aborted.
					}
					try {
						res.end();
					} catch {
						// Ignore write failures when the client has already aborted.
					}
				}
				return;
			}
			res.write(
				`data: ${JSON.stringify({
					type: "response.completed",
					response: {
						status: "completed",
						usage: {
							input_tokens: 10,
							output_tokens: 1,
							input_tokens_details: { cached_tokens: 0 },
						},
					},
				})}\n\n`,
			);
			res.write("data: [DONE]\n\n");
			res.end();
		});
	});

	await new Promise((resolve) => {
		server.listen(0, "127.0.0.1", resolve);
	});

	const address = server.address();
	if (!address || typeof address === "string") {
		throw new Error("Failed to start local responses server");
	}

	return {
		baseUrl: `http://127.0.0.1:${address.port}/v1`,
		async close() {
			server.close();
			await Promise.race([once(server, "close"), delay(200)]);
		},
	};
}

function shQuote(value) {
	return `'${String(value).replace(/'/g, `'\\''`)}'`;
}

function parseArgs(argv) {
	const result = {
		runtime: "both",
		width: 80,
		height: 24,
		rustBin: process.env.CELL_BIN || DEFAULT_RUST_BIN,
		scenarios: [],
	};

	for (let index = 0; index < argv.length; index += 1) {
		const arg = argv[index];
		if (arg === "--runtime" && index + 1 < argv.length) {
			result.runtime = argv[++index];
		} else if (arg === "--width" && index + 1 < argv.length) {
			result.width = Number.parseInt(argv[++index], 10);
		} else if (arg === "--height" && index + 1 < argv.length) {
			result.height = Number.parseInt(argv[++index], 10);
		} else if (arg === "--rust-bin" && index + 1 < argv.length) {
			result.rustBin = resolve(argv[++index]);
		} else if (arg === "--scenario" && index + 1 < argv.length) {
			result.scenarios.push(argv[++index]);
		} else if (arg === "--help" || arg === "-h") {
			printHelp();
			process.exit(0);
		} else {
			throw new Error(`Unknown argument: ${arg}`);
		}
	}

	if (!Number.isFinite(result.width) || result.width <= 0) {
		throw new Error(`Invalid width: ${result.width}`);
	}
	if (!Number.isFinite(result.height) || result.height <= 0) {
		throw new Error(`Invalid height: ${result.height}`);
	}
	if (!["ts", "rust", "both"].includes(result.runtime)) {
		throw new Error(`Unsupported runtime: ${result.runtime}`);
	}
	if (result.scenarios.length === 0) {
		result.scenarios = [...DEFAULT_SCENARIOS];
	}
	for (const scenario of result.scenarios) {
		if (!SUPPORTED_SCENARIOS.includes(scenario)) {
			throw new Error(`Unsupported scenario: ${scenario}`);
		}
	}
	return result;
}

function printHelp() {
	process.stdout.write(`Usage: node rust/scripts/tui_parity_runner.mjs [options]

Options:
  --runtime ts|rust|both   Which runtime(s) to capture (default: both)
  --scenario <name>        Scenario to capture. Repeatable.
  --width <cols>           PTY width (default: 80)
  --height <rows>          PTY height (default: 24)
  --rust-bin <path>        Rust binary path (default: ${DEFAULT_RUST_BIN})

TS parity captures require PI_TS_REPO=<path-to-typescript-repo>.
`);
}

function runCommand(command, args, options = {}) {
	const result = spawnSync(command, args, {
		cwd: options.cwd,
		env: options.env,
		input: options.input,
		encoding: "utf8",
	});
	if (result.error) {
		throw result.error;
	}
	if (options.check !== false && result.status !== 0) {
		const stderr = (result.stderr || "").trim();
		throw new Error(`${command} ${args.join(" ")} failed (${result.status})${stderr ? `: ${stderr}` : ""}`);
	}
	return result;
}

function haveCommand(command) {
	const result = spawnSync("sh", ["-lc", `command -v ${command}`], { encoding: "utf8" });
	return result.status === 0;
}

function nowIsoTimestamp() {
	return new Date().toISOString();
}

function zeroUsage() {
	return {
		input: 0,
		output: 0,
		cacheRead: 0,
		cacheWrite: 0,
		totalTokens: 0,
		cost: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			total: 0,
		},
	};
}

function encodeSessionDir(cwd) {
	return `--${cwd.replace(/^[/\\]/, "").replace(/[/\\:]/g, "-")}--`;
}

function sessionDirForCwd(homeDir, cwd) {
	return join(homeDir, ".pi", "agent", "sessions", encodeSessionDir(cwd));
}

function ensureDir(dir) {
	mkdirSync(dir, { recursive: true });
}

function writeJsonlFile(filePath, entries, modifiedAt) {
	ensureDir(dirname(filePath));
	writeFileSync(filePath, `${entries.map((entry) => JSON.stringify(entry)).join("\n")}\n`, "utf8");
	if (modifiedAt) {
		const time = new Date(modifiedAt);
		utimesSync(filePath, time, time);
	}
}

function assistantToolCallMessage(toolCallId, toolName, args, timestamp) {
	return {
		role: "assistant",
		content: [
			{
				type: "toolCall",
				id: toolCallId,
				name: toolName,
				arguments: args,
			},
		],
		api: "openai-responses",
		provider: "openai",
		model: "gpt-4.1",
		usage: zeroUsage(),
		stopReason: "toolUse",
		timestamp,
	};
}

function toolResultMessage(toolCallId, toolName, text, timestamp, details) {
	const result = {
		role: "toolResult",
		toolCallId,
		toolName,
		content: [
			{
				type: "text",
				text,
			},
		],
		isError: false,
		timestamp,
	};
	if (details !== undefined) {
		result.details = details;
	}
	return result;
}

function assistantTextMessage(text, timestamp) {
	return {
		role: "assistant",
		content: [{ type: "text", text }],
		api: "openai-responses",
		provider: "openai",
		model: "gpt-4.1",
		usage: zeroUsage(),
		stopReason: "stop",
		timestamp,
	};
}

function userTextMessage(text, timestamp) {
	return {
		role: "user",
		content: text,
		timestamp,
	};
}

function makeRichSessionEntries() {
	const now = Date.now();
	const timestamp = (offset) => new Date(now + offset * 1000).toISOString();
	return [
		{
			type: "message",
			id: "u1",
			parentId: null,
			timestamp: timestamp(1),
			message: userTextMessage("First user message", 1),
		},
		{
			type: "message",
			id: "a1",
			parentId: "u1",
			timestamp: timestamp(2),
			message: assistantTextMessage("Assistant response", 2),
		},
		{
			type: "session_info",
			id: "info1",
			parentId: "a1",
			timestamp: timestamp(3),
			name: "Paritied Session",
		},
		{
			type: "custom_message",
			id: "custom1",
			parentId: "info1",
			timestamp: timestamp(4),
			customType: "demo",
			content: "Custom extension message",
			display: true,
			details: { kind: "demo" },
		},
		{
			type: "message",
			id: "u2",
			parentId: "a1",
			timestamp: timestamp(5),
			message: userTextMessage(
				'<skill name="checks" location="/tmp/checks/SKILL.md">\nRun the checks.\n</skill>\n\nPlease run the checks.',
				5,
			),
		},
		{
			type: "message",
			id: "a2",
			parentId: "u2",
			timestamp: timestamp(6),
			message: assistantTextMessage("Branch body", 6),
		},
		{
			type: "branch_summary",
			id: "branch1",
			parentId: "u1",
			timestamp: timestamp(7),
			fromId: "u1",
			summary: "Branch summary body",
		},
		{
			type: "compaction",
			id: "compact1",
			parentId: "branch1",
			timestamp: timestamp(8),
			summary: "Compaction summary body",
			firstKeptEntryId: "u1",
			tokensBefore: 1234,
		},
		{
			type: "message",
			id: "u3",
			parentId: "compact1",
			timestamp: timestamp(9),
			message: userTextMessage("Follow-up after compaction", 9),
		},
	];
}

function makeResumeSessionEntries(name, firstMessage, timestampOffset = 0) {
	const now = Date.now() + timestampOffset * 1000;
	const timestamp = (offset) => new Date(now + offset * 1000).toISOString();
	return [
		{
			type: "message",
			id: "u1",
			parentId: null,
			timestamp: timestamp(1),
			message: userTextMessage(firstMessage, 1),
		},
		{
			type: "session_info",
			id: "info1",
			parentId: "u1",
			timestamp: timestamp(2),
			name,
		},
	];
}

function writeResourceFixtures(homeDir) {
	const agentDir = join(homeDir, ".pi", "agent");
	ensureDir(join(agentDir, "skills", "checks"));
	ensureDir(join(agentDir, "prompts"));
	ensureDir(join(agentDir, "themes"));
	const skillPath = join(agentDir, "skills", "checks", "SKILL.md");
	const promptPath = join(agentDir, "prompts", "brief.md");
	const themePath = join(agentDir, "themes", "broken.json");
	writeFileSync(
		skillPath,
		`---
name: checks
description: Run verification checks
---
Use the checks skill.
`,
		"utf8",
	);
	writeFileSync(
		promptPath,
		`---
description: Brief prompt
---
Prompt body.
`,
		"utf8",
	);
	writeFileSync(themePath, "{", "utf8");
	return { skillPath, promptPath, themePath };
}

function writeAuthFixtures(homeDir) {
	const agentDir = join(homeDir, ".pi", "agent");
	ensureDir(agentDir);
	writeFileSync(
		join(agentDir, "auth.json"),
		JSON.stringify(
			{
				anthropic: {
					type: "oauth",
					refresh: "refresh-anthropic",
					access: "access-anthropic",
					expires: Date.now() + 60 * 60 * 1000,
				},
				"openai-codex": {
					type: "oauth",
					refresh: "refresh-openai-codex",
					access: "access-openai-codex",
					expires: Date.now() + 60 * 60 * 1000,
				},
			},
			null,
			2,
		),
		"utf8",
	);
}

function prepareScenarioFixtures(tempRoot, scenario, cwd, liveBaseUrl) {
	const homeDir = join(tempRoot, "home");
	const fixtures = {
		sessionPath: undefined,
		extraArgs: [],
		extraEnv: {},
		extraPathEntries: [],
	};

	if (scenario === "startup-diagnostics" || scenario === "reload-diagnostics") {
		const resourcePaths = writeResourceFixtures(homeDir);
		fixtures.extraArgs.push(
			"--skill",
			resourcePaths.skillPath,
			"--prompt-template",
			resourcePaths.promptPath,
			"--theme",
			resourcePaths.themePath,
		);
	}

	if (scenario === "login" || scenario === "logout") {
		writeAuthFixtures(homeDir);
	}

	if (scenario === "footer-subscription") {
		writeAuthFixtures(homeDir);
	}

	if (liveBaseUrl) {
		writeModelsFixture(homeDir, liveBaseUrl);
	}

	if (scenario === "footer-unknown-context") {
		writeUnknownContextModelsFixture(homeDir);
	}

	if (scenario === "config-browser") {
		const packageRoot = join(tempRoot, "local-package");
		ensureDir(packageRoot);
		ensureDir(join(packageRoot, "skills"));
		writeFileSync(
			join(packageRoot, "skills", "resource.md"),
			"---\ndescription: Local package skill\n---\nUse the local package resource.\n",
			"utf8",
		);
		writeSettingsFixture(homeDir, { packages: [packageRoot] });
	}

	if (scenario === "share-success") {
		fixtures.extraPathEntries.push(writeGhFixture(tempRoot, "success"));
		fixtures.extraEnv.PI_SHARE_VIEWER_URL = "https://share.example/viewer";
	}

	if (scenario === "share-cancel") {
		fixtures.extraPathEntries.push(writeGhFixture(tempRoot, "cancel"));
		fixtures.extraEnv.PI_SHARE_VIEWER_URL = "https://share.example/viewer";
	}

	if (scenario === "hidden-thinking") {
		writeSettingsFixture(homeDir, { hideThinkingBlock: true });
	}

	if (scenario === "resume-populated-management") {
		const currentSessionDir = sessionDirForCwd(homeDir, cwd);
		const otherSessionDir = sessionDirForCwd(homeDir, join(tempRoot, "other-project"));
		writeJsonlFile(
			join(currentSessionDir, "alpha.jsonl"),
			[
				{ type: "session", version: 3, id: "alpha", timestamp: new Date(Date.now() - 5000).toISOString(), cwd },
				...makeResumeSessionEntries("Alpha Session", "Alpha message"),
			],
			new Date(Date.now() - 5000).toISOString(),
		);
		writeJsonlFile(
			join(currentSessionDir, "beta.jsonl"),
			[
				{ type: "session", version: 3, id: "beta", timestamp: new Date(Date.now() - 3000).toISOString(), cwd },
				...makeResumeSessionEntries("", "Beta message", 10),
			],
			new Date(Date.now() - 3000).toISOString(),
		);
		writeJsonlFile(
			join(otherSessionDir, "gamma.jsonl"),
			[
				{
					type: "session",
					version: 3,
					id: "gamma",
					timestamp: new Date(Date.now() - 1000).toISOString(),
					cwd: join(tempRoot, "other-project"),
				},
				...makeResumeSessionEntries("Gamma Session", "Gamma message", 20),
			],
			new Date(Date.now() - 1000).toISOString(),
		);
	}

	if (SESSION_SCENARIOS.has(scenario) && !SEEDED_TOOL_SCENARIOS.has(scenario)) {
		const sessionDir = sessionDirForCwd(homeDir, cwd);
		const sessionPath = join(sessionDir, `${scenario}.jsonl`);
		writeJsonlFile(
			sessionPath,
			[
				{
					type: "session",
					version: 3,
					id: randomUUID(),
					timestamp: nowIsoTimestamp(),
					cwd,
				},
				...makeRichSessionEntries(),
			],
		);
		fixtures.sessionPath = sessionPath;
	}

	return fixtures;
}

function createSeededSessionFile(tempRoot, scenario, runtimeRoot) {
	const sessionPath = join(tempRoot, `${scenario}.jsonl`);
	const sessionId = randomUUID();
	const baseTimestamp = nowIsoTimestamp();
	const nextTimestamp = (offset) => new Date(Date.now() + offset * 1000).toISOString();

	const header = {
		type: "session",
		version: 3,
		id: sessionId,
		timestamp: baseTimestamp,
		cwd: runtimeRoot,
	};

	let entries;
	switch (scenario) {
		case "read":
			entries = [
				{
					type: "message",
					id: "read-assistant",
					parentId: null,
					timestamp: nextTimestamp(1),
					message: assistantToolCallMessage(
						"call-read",
						"read",
						{ path: "/tmp/example.rs", offset: 2, limit: 2 },
						1,
					),
				},
				{
					type: "message",
					id: "read-result",
					parentId: "read-assistant",
					timestamp: nextTimestamp(2),
					message: toolResultMessage(
						"call-read",
						"read",
						"fn answer() {}\nreturn 42;\n\n[2 more lines in file. Use offset=4 to continue.]",
						2,
					),
				},
			];
			break;
		case "write":
			entries = [
				{
					type: "message",
					id: "write-assistant",
					parentId: null,
					timestamp: nextTimestamp(1),
					message: assistantToolCallMessage(
						"call-write",
						"write",
						{
							path: "src/main.rs",
							content: 'fn main() {\n    println!("hi");\n}',
						},
						1,
					),
				},
				{
					type: "message",
					id: "write-result",
					parentId: "write-assistant",
					timestamp: nextTimestamp(2),
					message: toolResultMessage(
						"call-write",
						"write",
						"Successfully wrote 31 bytes to src/main.rs",
						2,
					),
				},
			];
			break;
		case "edit":
			entries = [
				{
					type: "message",
					id: "edit-assistant",
					parentId: null,
					timestamp: nextTimestamp(1),
					message: assistantToolCallMessage(
						"call-edit",
						"edit",
						{
							path: "src/lib.rs",
							oldText: "let value = 1;",
							newText: "let value = 2;",
						},
						1,
					),
				},
				{
					type: "message",
					id: "edit-result",
					parentId: "edit-assistant",
					timestamp: nextTimestamp(2),
					message: toolResultMessage(
						"call-edit",
						"edit",
						"Updated src/lib.rs",
						2,
						{ diff: "@@ -1 +1 @@\n-let value = 1;\n+let value = 2;" },
					),
				},
			];
			break;
		default:
			throw new Error(`Unsupported seeded session scenario: ${scenario}`);
	}

	const content = [header, ...entries].map((entry) => JSON.stringify(entry)).join("\n");
	writeFileSync(sessionPath, `${content}\n`, "utf8");
	return sessionPath;
}

function currentBranch(cwd) {
	try {
		return runCommand("git", ["rev-parse", "--abbrev-ref", "HEAD"], { cwd }).stdout.trim();
	} catch {
		return "";
	}
}

function makeLauncher(runtime, options, runtimeRoot, tempRoot) {
	const homeDir = join(tempRoot, "home");
	mkdirSync(homeDir, { recursive: true });

	const envLines = [
		`export HOME=${shQuote(homeDir)}`,
		`export XDG_CONFIG_HOME=${shQuote(join(homeDir, ".config"))}`,
		`export XDG_CACHE_HOME=${shQuote(join(homeDir, ".cache"))}`,
		`export CELL_CODING_AGENT_DIR=${shQuote(join(homeDir, ".pi", "agent"))}`,
		"export OPENAI_API_KEY='fake-openai-key'",
	];
	if (options.extraEnv) {
		for (const [key, value] of Object.entries(options.extraEnv)) {
			envLines.push(`export ${key}=${shQuote(value)}`);
		}
	}
	if (options.extraPathEntries && options.extraPathEntries.length > 0) {
		const pathEntries = [...options.extraPathEntries, process.env.PATH || ""].filter(Boolean);
		envLines.push(`export PATH=${shQuote(pathEntries.join(":"))}`);
	}
	for (const name of API_ENV_VARS) {
		if (name === "OPENAI_API_KEY") {
			continue;
		}
		envLines.push(`unset ${name}`);
	}

	const executable =
		runtime === "ts"
			? `node ${shQuote(tsxCliPath())} ${shQuote(tsCliPath())}`
			: shQuote(options.rustBin);
	const args = scenarioLauncherArgs(options.scenario, options.sessionPath, options.extraArgs)
		.map((value) => shQuote(value))
		.join(" ");
	const launcherPath = join(tempRoot, `launch-${runtime}.sh`);
	const content = [
		"#!/usr/bin/env bash",
		"set -euo pipefail",
		...envLines,
		`cd ${shQuote(runtimeRoot)}`,
		`exec ${executable} ${args}`,
		"",
	].join("\n");
	writeFileSync(launcherPath, content, "utf8");
	chmodSync(launcherPath, 0o755);
	return launcherPath;
}

function tmux(...args) {
	return runCommand("tmux", args, { check: false });
}

function sendLiteral(session, text) {
	runCommand("tmux", ["send-keys", "-t", session, "-l", "--", text]);
}

function sendKey(session, key) {
	runCommand("tmux", ["send-keys", "-t", session, key]);
}

function capturePane(session) {
	return runCommand("tmux", ["capture-pane", "-t", session, "-p"], { check: false }).stdout.replace(/\r/g, "");
}

function capturePaneAnsi(session) {
	return runCommand("tmux", ["capture-pane", "-e", "-t", session, "-p"], {
		check: false,
	}).stdout.replace(/\r/g, "");
}

function captureScenarioSnapshot(session, runtime, scenario, runtimeRoot, tempRoot, branch, extra = {}) {
	return buildCaptureRecord(
		runtime,
		scenario,
		capturePane(session),
		capturePaneAnsi(session),
		runtimeRoot,
		tempRoot,
		branch,
		extra,
	);
}

async function sleep(ms) {
	await new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForCapture(session, predicate, timeoutMs = 8000) {
	const startedAt = Date.now();
	let last = "";
	while (Date.now() - startedAt < timeoutMs) {
		last = capturePane(session);
		if (predicate(last)) {
			return last;
		}
		await sleep(250);
	}
	return last;
}

function bootstrapReady(runtime, text) {
	if (runtime === "ts") {
		return text.includes("gpt-4.1") && text.includes("<") === false;
	}
	return text.includes("gpt-4.1");
}

function hasStableFooter(text) {
	return text.includes("gpt-4.1") && text.includes("0.0%/128k (auto)");
}

function readyForScenario(runtime, scenario, text) {
	if (/panicked at/.test(text)) {
		return true;
	}

	switch (`${runtime}:${scenario}`) {
		case "ts:startup":
			return text.includes("gpt-4.1");
		case "rust:startup":
			return text.includes("gpt-4.1") && text.includes("[Context]");
		case "ts:slash":
			return text.includes("settings                        Open settings menu");
		case "rust:slash":
			return text.includes("Type a slash command") || /panicked at/.test(text);
		case "ts:model":
			return text.includes("Model Name: GPT-4.1");
		case "rust:model":
			return text.includes("Model Name: GPT-4.1") || /panicked at/.test(text);
		case "ts:resume":
			return text.includes("Resume Session");
		case "rust:resume":
			return text.includes("Resume Session");
		case "ts:settings":
			return text.includes("Settings");
		case "rust:settings":
			return text.includes("Auto-compact");
		case "ts:startup-resume":
		case "rust:startup-resume":
			return text.includes("Resume Session");
		case "ts:config-browser":
		case "rust:config-browser":
			return text.includes("Resource Configuration");
		case "ts:scoped-models":
		case "rust:scoped-models":
			return (
				(text.includes("Model Configuration") && text.includes("Model Name:")) ||
				/panicked at/.test(text)
			);
		case "ts:hidden-thinking":
		case "rust:hidden-thinking":
			return text.includes("Thinking...") || text.includes("Hidden thinking response");
		case "ts:live-streaming-start":
		case "rust:live-streaming-start":
			return (
				text.includes("Live streaming start") &&
				text.includes("Streaming visible thinking start")
			);
		case "ts:live-streaming-mid":
		case "rust:live-streaming-mid":
			return (
				text.includes("Live streaming mid") &&
				text.includes("Streaming visible thinking mid")
			);
		case "ts:abort-active-run":
		case "rust:abort-active-run":
			return text.includes("Request aborted") || text.includes("Operation aborted");
		case "ts:tool-lifecycle":
		case "rust:tool-lifecycle":
			return text.includes("tool-lifecycle complete") || text.includes("$ printf tool-lifecycle");
		case "ts:startup-diagnostics":
		case "rust:startup-diagnostics":
			return text.includes("[Context]") && text.includes("[Skills]") && text.includes("[Themes]");
		case "ts:resume-populated-management":
		case "rust:resume-populated-management":
			return (
				text.includes("Resume Session") &&
				(
					text.includes("Alpha Session") ||
					text.includes("Beta Session") ||
					text.includes("Gamma Session") ||
					text.includes("No sessions found") ||
					text.includes("No sessions in current folder. Press Tab to view all.")
				)
			);
		case "ts:tree-navigation":
		case "rust:tree-navigation":
			return text.includes("Session Tree") && text.includes("Type to search:");
		case "ts:tree-summary":
		case "rust:tree-summary":
			return text.includes("Summarize branch?");
		case "ts:fork-populated":
		case "rust:fork-populated":
			return text.includes("Branch from Message") && text.includes("Message 1 of");
		case "ts:login":
		case "rust:login":
			return text.includes("Select provider to login:");
		case "ts:logout":
		case "rust:logout":
			return text.includes("Select provider to logout:") || text.includes("No OAuth providers logged in. Use /login first.");
		case "ts:reload-diagnostics":
		case "rust:reload-diagnostics":
			return (
				text.includes("Reloaded extensions, skills, prompts, themes") ||
				text.includes("Reloaded with") ||
				text.includes("Check your settings files.")
			);
		case "ts:session":
		case "rust:session":
			return (
				text.includes("Session Info") ||
				text.includes("Session Info added to the transcript.")
			);
		case "ts:changelog":
		case "rust:changelog":
			return (
				text.includes("What's New") ||
				text.includes("Changelog added to the transcript.") ||
				text.includes("### Added") ||
				text.includes("### Changed")
			);
		case "ts:hotkeys":
		case "rust:hotkeys":
			return (
				text.includes("Keyboard Shortcuts") ||
				text.includes("Keyboard shortcuts added to the transcript.")
			);
		case "ts:copy-empty":
		case "rust:copy-empty":
			return text.includes("No agent messages to copy yet.");
		case "ts:share-missing-gh":
		case "rust:share-missing-gh":
			return (
				text.includes("GitHub CLI is not logged in. Run 'gh auth login' first.") ||
				text.includes("GitHub CLI (gh) is not installed. Install it from https://cli.github.com/")
			);
		case "ts:footer-variants":
		case "rust:footer-variants":
			return text.includes("gpt-4.1");
		case "ts:footer-subscription":
		case "rust:footer-subscription":
			return text.includes("gpt-5.3-codex");
		case "ts:footer-unknown-context":
		case "rust:footer-unknown-context":
			return text.includes("?/0");
		case "ts:excluded-bash":
		case "rust:excluded-bash":
			return text.includes("Excluded from context") || text.includes("hello-from-bash");
		case "ts:custom-messages-and-skills":
		case "rust:custom-messages-and-skills":
			return text.includes("[demo]") && text.includes("[skill]");
		case "ts:compaction-and-retry":
		case "rust:compaction-and-retry":
			return text.includes("[compaction]") || text.includes("Compacted from");
		case "ts:shift-tab":
			return text.includes("Current model does not support thinking");
		case "rust:shift-tab":
			return text.includes("Thinking level: minimal");
		case "ts:fork":
		case "rust:fork":
			return text.includes("No messages to fork from");
		case "ts:manual-bash":
		case "rust:manual-bash":
			return text.includes("!printf hello-from-bash");
		case "ts:read":
		case "rust:read":
			return text.includes("read /tmp/example.rs:2-3") && text.includes("return 42;");
		case "ts:write":
		case "rust:write":
			return text.includes("write src/main.rs") && text.includes('println!("hi");');
		case "ts:edit":
		case "rust:edit":
			return text.includes("edit src/lib.rs") && text.includes("- let value = 1;") && text.includes("+ let value = 2;");
		case "ts:sticky-footer":
		case "rust:sticky-footer":
			return text.includes("line-40");
		case "ts:bash":
			return text.includes("hello-from-bash");
		case "rust:bash":
			return text.includes("hello-from-bash");
		case "ts:diff":
			return text.includes("line-three") && text.includes("(exit 1)");
		case "rust:diff":
			return text.includes("line-three") && text.includes("@@");
		default:
			return false;
	}
}

function normalizeText(text, runtimeRoot, tempRoot, branch) {
	let normalized = text.replaceAll(runtimeRoot, "<REPO>").replaceAll(tempRoot, "<TMP>");
	normalized = normalized.replace(/cell v\d+\.\d+\.\d+/g, "cell v<VERSION>");
	normalized = normalized.replace(/π v\d+\.\d+\.\d+/g, "π v<VERSION>");
	normalized = normalized.replace(
		/\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/gi,
		"<SESSION_ID>",
	);
	if (branch) {
		normalized = normalized.replaceAll(`git:${branch}`, "git:<BRANCH>");
		normalized = normalized.replaceAll(`(${branch})`, "(<BRANCH>)");
	}
	return normalized
		.split("\n")
		.map((line) => line.replace(/\s+$/g, ""))
		.join("\n")
		.trimEnd();
}

function isBootstrapNoiseLine(line, previousWasLauncherEcho) {
	const trimmed = line.trim();
	if (trimmed.length === 0) {
		return false;
	}
	if (/^<TMP>\/launch-[^/]+\.sh$/.test(trimmed)) {
		return true;
	}
	if (/^[^%]* pi % .*launch-/.test(line)) {
		return true;
	}
	if (previousWasLauncherEcho && /^(?:-)?(?:ts|rust)\.sh$/.test(trimmed)) {
		return true;
	}
	if (trimmed.includes("fd not found. Downloading...")) {
		return true;
	}
	if (trimmed.includes("fd installed to ") && trimmed.includes("/.pi/agent")) {
		return true;
	}
	if (trimmed === "/bin/fd" || trimmed === "t/bin/fd") {
		return true;
	}
	return false;
}

function appOwnedStartIndex(lines) {
	let previousWasLauncherEcho = false;
	for (let index = 0; index < lines.length; index += 1) {
		const line = lines[index];
		if (isBootstrapNoiseLine(line, previousWasLauncherEcho)) {
			previousWasLauncherEcho =
				line.includes(" pi % ") && line.includes("launch-");
			continue;
		}
		const trimmed = line.trimStart();
		if (
			trimmed.startsWith("│") ||
			trimmed.startsWith("╭") ||
			trimmed.startsWith("╰") ||
			isOverlayBannerLine(trimmed) ||
			trimmed.startsWith("π v") ||
			trimmed.startsWith("pi v") ||
			trimmed.startsWith("escape to interrupt") ||
			trimmed.startsWith("ctrl+c to clear") ||
			trimmed.startsWith("ctrl+c twice to exit") ||
			trimmed.startsWith("ctrl+d to exit (empty)") ||
			trimmed.startsWith("ctrl+z to suspend") ||
			trimmed.startsWith("ctrl+k to delete to end") ||
			trimmed.startsWith("shift+tab to cycle thinking level") ||
			trimmed.startsWith("ctrl+p/shift+ctrl+p to cycle models") ||
			trimmed.startsWith("ctrl+l to select model") ||
			trimmed.startsWith("ctrl+o to expand tools") ||
			trimmed.startsWith("ctrl+t to expand thinking") ||
			trimmed.startsWith("ctrl+g for external editor") ||
			trimmed.startsWith("/ for commands") ||
			trimmed.startsWith("! to run bash") ||
			trimmed.startsWith("!! to run bash (no context)") ||
			trimmed.startsWith("alt+enter to queue follow-up") ||
			trimmed.startsWith("alt+up to edit all queued messages") ||
			trimmed.startsWith("ctrl+v to paste image") ||
			trimmed.startsWith("drop files to attach") ||
			trimmed.startsWith("[Context]") ||
			trimmed.startsWith("[Skills]") ||
			trimmed.startsWith("[Prompts]") ||
			trimmed.startsWith("[Extensions]") ||
			trimmed.startsWith("[Themes]") ||
			trimmed.startsWith("Session Info") ||
			trimmed.startsWith("What's New") ||
			trimmed.startsWith("Keyboard Shortcuts") ||
			trimmed.startsWith("Reloaded extensions, skills, prompts, themes") ||
			trimmed.startsWith("✓ New session started") ||
			trimmed.startsWith("GitHub CLI is not logged in.") ||
			trimmed.startsWith("GitHub CLI (gh) is not installed.") ||
			trimmed.startsWith("No agent messages to copy yet.") ||
			trimmed.startsWith("Select provider to login:") ||
			trimmed.startsWith("Select provider to logout:") ||
			trimmed.startsWith("No OAuth providers logged in. Use /login first.") ||
			trimmed.startsWith("Resource Configuration") ||
			trimmed.startsWith("Type to filter resources") ||
			trimmed.startsWith("Resume Session") ||
			trimmed.startsWith("Only showing models with configured API keys") ||
			trimmed.startsWith("Scope: ") ||
			trimmed.startsWith("Model Configuration") ||
			trimmed.startsWith("Branch from Message") ||
			trimmed.startsWith("No messages to fork from") ||
			trimmed.startsWith("Session Tree") ||
			trimmed.startsWith("Type to search:") ||
			trimmed.startsWith("Label (empty to remove):") ||
			trimmed.startsWith("Summarize branch?") ||
			trimmed.startsWith("No summary") ||
			trimmed.startsWith("Summarize with custom prompt") ||
			trimmed.startsWith("Branch summarization cancelled") ||
			trimmed.startsWith("Delete session? [Enter] confirm") ||
			trimmed.startsWith("Compacting context...") ||
			trimmed.startsWith("Auto-compacting context...") ||
			trimmed.startsWith("Retrying (") ||
			trimmed.startsWith("[branch]") ||
			trimmed.startsWith("[compaction]") ||
			trimmed.startsWith("[skill]") ||
			trimmed.startsWith("[demo]") ||
			trimmed.startsWith("Compacted from ") ||
			trimmed.startsWith("→ ") ||
			trimmed.startsWith("Thinking level:") ||
			trimmed.startsWith("Working...") ||
			trimmed.startsWith("· Working...") ||
			trimmed.startsWith("• Working...") ||
			trimmed.startsWith("∙ Working...") ||
			trimmed.startsWith("Live streaming start") ||
			trimmed.startsWith("Live streaming mid") ||
			trimmed.startsWith("Aborting live run") ||
			trimmed.startsWith("Hidden thinking response") ||
			trimmed.startsWith("Thinking...") ||
			trimmed.startsWith("tool-lifecycle complete") ||
			trimmed.startsWith("read ") ||
			trimmed.startsWith("write ") ||
			trimmed.startsWith("edit ") ||
			trimmed.startsWith("diff --git") ||
			trimmed.startsWith("$ ") ||
			trimmed.startsWith("hello-from-bash") ||
			trimmed.startsWith("$ ") ||
			trimmed === ">" ||
			isDividerLine(trimmed)
		) {
			return index;
		}
	}
	return 0;
}

function isOverlayBannerLine(line) {
	return (
		line.startsWith("╭─ ") &&
		(
			line.includes("Session Tree") ||
			line.includes("Summarize branch?") ||
			line.includes("Branch from Message") ||
			line.includes("Select provider to login:") ||
			line.includes("Select provider to logout:") ||
			line.includes("Resume Session") ||
			line.includes("Model Configuration") ||
			line.includes("Scoped Models") ||
			line.includes("Custom summarization instructions") ||
			line.includes("Rename Session")
		)
	);
}

function isDividerLine(line) {
	const trimmed = line.trim();
	return trimmed.length > 0 && /^─+$/.test(trimmed);
}

function summarizeCapture(text) {
	const lines = text.split("\n");
	const nonEmpty = lines.filter((line) => line.trim().length > 0);
	const promptRow = lines.findLastIndex((line) => line.startsWith(">"));
	const normalized = text.replace(/\s+/g, " ");
	const contains = (needle) => text.includes(needle);
	const containsNormalized = (needle) => normalized.includes(needle);
	return {
		lines,
		lineCount: lines.length,
		footerTail: nonEmpty.slice(-6),
		contextBlockPresent: lines.some((line) => line.includes("[Context]")),
		promptRow,
		dividerRows: lines.flatMap((line, index) => (isDividerLine(line) ? [index] : [])),
		bottomHelpPresent:
			promptRow >= 0 ? lines.slice(promptRow + 1).some((line) => /enter sends/i.test(line)) : false,
		containsNoMessagesYet: contains("No messages yet."),
		containsSelectorOpenStatus: contains("selector open."),
		containsBashTitle: contains("╭─ Bash"),
		containsExitCodeZero: contains("Exit code: 0"),
		containsRanBashStatus: contains("ran bash:"),
		containsExcludedFromContext: contains("Excluded from context"),
		containsCtrlGHint: contains("ctrl+g for external editor"),
		containsCtrlVHint: contains("ctrl+v to paste image"),
		containsDropFilesHint: contains("drop files to attach"),
		containsThinkingLevelStatus:
			contains("Thinking level:") || contains("Current model does not support thinking"),
		containsNoMessagesToForkFrom: containsNormalized("No messages to fork from"),
		containsSessionInfoTitle: contains("Session Info"),
		containsWhatsNewTitle: contains("What's New"),
		containsKeyboardShortcutsTitle: contains("Keyboard Shortcuts"),
		containsReloadedResourcesStatus: contains("Reloaded extensions, skills, prompts, themes"),
		containsGitHubCliNotLoggedIn: contains("GitHub CLI is not logged in. Run 'gh auth login' first."),
		containsGitHubCliNotInstalled: contains(
			"GitHub CLI (gh) is not installed. Install it from https://cli.github.com/",
		),
		containsNoAgentMessagesToCopyYet: contains("No agent messages to copy yet."),
		containsOAuthLoginTitle: contains("Select provider to login:"),
		containsOAuthLogoutTitle: contains("Select provider to logout:"),
		containsNoOAuthProvidersLoggedIn: contains("No OAuth providers logged in. Use /login first."),
		containsSessionTreeTitle: contains("Session Tree"),
		containsTreeSearchPrompt: contains("Type to search:"),
		containsTreeLabelPrompt: contains("Label (empty to remove):"),
		containsSummarizeBranchPrompt: contains("Summarize branch?"),
		containsBranchSummarizationCancelled: contains("Branch summarization cancelled"),
		containsBranchFromMessageTitle: contains("Branch from Message"),
		containsBranchFromMessageSubtitle: contains("Select a message to create a new branch from that point"),
		containsDeleteSessionPrompt: contains("Delete session? [Enter] confirm"),
		containsReloadedResources: contains("Reloaded extensions, skills, prompts, themes"),
		containsCompactionLabel: contains("[compaction]"),
		containsBranchLabel: contains("[branch]"),
		containsSkillLabel: contains("[skill]"),
		containsCustomDemoLabel: contains("[demo]"),
		containsCompactedFrom: contains("Compacted from "),
		containsRetryingStatus: contains("Retrying ("),
		containsFooterThinkingDetail: contains("thinking off"),
		containsFooterModelOnly: contains("gpt-4.1") && !contains("thinking off"),
		crashed: /panicked at/.test(text),
		shellFallback: /@\S+.*[%#]\s*$/.test(text),
	};
}

const SELECTOR_SHELL_TITLES = [
	"Resume Session",
	"Resume Sessi",
	"Session Tree",
	"Select provider to login:",
	"Select provider to logout:",
	"Resource Configuration",
	"Model Configuration",
	"Scoped Models",
	"Branch from Message",
	"Summarize branch?",
	"Rename Session",
	"Session Info",
	"Settings",
	"Custom summarization instructions",
	"What's New",
	"Keyboard Shortcuts",
];

const STATUS_LINE_PREFIXES = [
	"Working...",
	"· Working...",
	"• Working...",
	"∙ Working...",
	"selector open.",
	"Current model does not support thinking",
	"Thinking level:",
	"Request aborted",
	"Operation aborted",
	"No sessions found",
	"No sessions in current folder. Press Tab to view all.",
	"No resources found",
	"No OAuth providers logged in. Use /login first.",
	"GitHub CLI is not logged in.",
	"GitHub CLI (gh) is not installed.",
	"No agent messages to copy yet.",
	"Share URL:",
	"Share cancelled",
	"Creating gist...",
	"Failed to create gist",
	"Updated ",
	"Reloaded extensions, skills, prompts, themes",
	"Session Info added to the transcript.",
	"Changelog added to the transcript.",
	"Keyboard shortcuts added to the transcript.",
	"Branch summarization cancelled",
	"Session compacted ",
	"Compacted from ",
	"No messages to fork from",
];

function isSelectorShellTitleLine(line) {
	const trimmed = line.trim();
	return SELECTOR_SHELL_TITLES.some((title) => trimmed.includes(title));
}

function isStatusLine(line) {
	const trimmed = line.trim();
	return STATUS_LINE_PREFIXES.some((prefix) => trimmed.startsWith(prefix));
}

function findFooterStartIndex(lines) {
	let footerContentIndex = -1;
	let modelContentIndex = -1;
	for (let index = lines.length - 1; index >= 0; index -= 1) {
		const trimmed = lines[index].trim();
		if (trimmed.length === 0) {
			continue;
		}
		if (
			trimmed.includes("<REPO>") ||
			trimmed.includes("gpt-4.1") ||
			trimmed.includes("gpt-5") ||
			trimmed.includes("claude") ||
			trimmed.includes("auto)")
		) {
			if (trimmed.includes("<REPO>")) {
				footerContentIndex = index;
				break;
			}
			modelContentIndex = index;
		}
	}

	if (footerContentIndex < 0) {
		footerContentIndex = modelContentIndex;
	}

	if (footerContentIndex < 0) {
		return lines.length;
	}

	let footerStart = footerContentIndex;
	while (footerStart > 0) {
		const previous = lines[footerStart - 1];
		if (previous.trim().length === 0 || isDividerLine(previous)) {
			footerStart -= 1;
			continue;
		}
		break;
	}
	return footerStart;
}

function findSelectorShellStartIndex(lines) {
	for (let index = 0; index < lines.length; index += 1) {
		if (!isSelectorShellTitleLine(lines[index]) && !isOverlayBannerLine(lines[index].trimStart())) {
			continue;
		}

		let selectorStart = index;
		while (selectorStart > 0) {
			const previous = lines[selectorStart - 1];
			if (previous.trim().length === 0 || isDividerLine(previous)) {
				selectorStart -= 1;
				continue;
			}
			break;
		}
		return selectorStart;
	}

	return -1;
}

function findPromptRowIndex(lines) {
	for (let index = 0; index < lines.length; index += 1) {
		const trimmed = lines[index].trimStart();
		if (trimmed === ">" || trimmed.startsWith("> ")) {
			return index;
		}
	}
	return -1;
}

function sliceAppOwnedSections(text) {
	const lines = text.split("\n");
	const footerStart = findFooterStartIndex(lines);
	const selectorShellStartIndex = findSelectorShellStartIndex(lines);
	const promptRowIndex = findPromptRowIndex(lines);
	const transcriptEnd = selectorShellStartIndex >= 0 ? selectorShellStartIndex : footerStart;
	const composerStart = promptRowIndex >= 0 ? promptRowIndex : -1;
	const composerEnd = footerStart;
	const selectorShellEnd = footerStart;

	return {
		appOwnedTranscriptRows: lines.slice(0, transcriptEnd),
		appOwnedStatusRows: lines.filter(isStatusLine),
		appOwnedComposerRows: composerStart >= 0 ? lines.slice(composerStart, composerEnd) : [],
		appOwnedFooterRows: lines.slice(footerStart),
		appOwnedSelectorShellRows:
			selectorShellStartIndex >= 0 ? lines.slice(selectorShellStartIndex, selectorShellEnd) : [],
	};
}

function sliceAppOwnedCapture(text, ansiText) {
	const lines = text.split("\n");
	const ansiLines = ansiText.split("\n");
	const start = appOwnedStartIndex(lines);
	const appOwnedText = lines.slice(start).join("\n").trimEnd();
	const appOwnedAnsiText = ansiLines.slice(start).join("\n").trimEnd();
	return {
		appOwnedText,
		appOwnedAnsiText,
		appOwnedStartIndex: start,
		...sliceAppOwnedSections(appOwnedText),
	};
}

function buildCaptureRecord(runtime, scenario, text, ansiText, runtimeRoot, tempRoot, branch, extra = {}) {
	const normalized = normalizeText(text, runtimeRoot, tempRoot, branch);
	const normalizedAnsi = normalizeText(ansiText, runtimeRoot, tempRoot, branch);
	const appOwned = sliceAppOwnedCapture(normalized, normalizedAnsi);
	const appOwnedSummaries = summarizeCapture(appOwned.appOwnedText);
	return {
		runtime,
		scenario,
		...summarizeCapture(normalized),
		text: normalized,
		ansiText: normalizedAnsi,
		...appOwned,
		appOwnedLineCount: appOwnedSummaries.lineCount,
		appOwnedFooterTail: appOwnedSummaries.footerTail,
		appOwnedContextBlockPresent: appOwnedSummaries.contextBlockPresent,
		appOwnedPromptRow: appOwnedSummaries.promptRow,
		appOwnedDividerRows: appOwnedSummaries.dividerRows,
		appOwnedBottomHelpPresent: appOwnedSummaries.bottomHelpPresent,
		appOwnedContainsNoMessagesYet: appOwnedSummaries.containsNoMessagesYet,
		appOwnedContainsSelectorOpenStatus: appOwnedSummaries.containsSelectorOpenStatus,
		appOwnedContainsBashTitle: appOwnedSummaries.containsBashTitle,
		appOwnedContainsExitCodeZero: appOwnedSummaries.containsExitCodeZero,
		appOwnedContainsRanBashStatus: appOwnedSummaries.containsRanBashStatus,
		appOwnedContainsExcludedFromContext: appOwnedSummaries.containsExcludedFromContext,
		appOwnedContainsCtrlGHint: appOwnedSummaries.containsCtrlGHint,
		appOwnedContainsCtrlVHint: appOwnedSummaries.containsCtrlVHint,
		appOwnedContainsDropFilesHint: appOwnedSummaries.containsDropFilesHint,
		appOwnedContainsThinkingLevelStatus: appOwnedSummaries.containsThinkingLevelStatus,
		appOwnedContainsNoMessagesToForkFrom: appOwnedSummaries.containsNoMessagesToForkFrom,
		appOwnedContainsSessionInfoTitle: appOwnedSummaries.containsSessionInfoTitle,
		appOwnedContainsWhatsNewTitle: appOwnedSummaries.containsWhatsNewTitle,
		appOwnedContainsKeyboardShortcutsTitle: appOwnedSummaries.containsKeyboardShortcutsTitle,
		appOwnedContainsReloadedResourcesStatus: appOwnedSummaries.containsReloadedResourcesStatus,
		appOwnedContainsGitHubCliNotLoggedIn: appOwnedSummaries.containsGitHubCliNotLoggedIn,
		appOwnedContainsGitHubCliNotInstalled: appOwnedSummaries.containsGitHubCliNotInstalled,
		appOwnedContainsNoAgentMessagesToCopyYet: appOwnedSummaries.containsNoAgentMessagesToCopyYet,
		appOwnedContainsOAuthLoginTitle: appOwnedSummaries.containsOAuthLoginTitle,
		appOwnedContainsOAuthLogoutTitle: appOwnedSummaries.containsOAuthLogoutTitle,
		appOwnedContainsNoOAuthProvidersLoggedIn: appOwnedSummaries.containsNoOAuthProvidersLoggedIn,
		appOwnedContainsSessionTreeTitle: appOwnedSummaries.containsSessionTreeTitle,
		appOwnedContainsTreeSearchPrompt: appOwnedSummaries.containsTreeSearchPrompt,
		appOwnedContainsTreeLabelPrompt: appOwnedSummaries.containsTreeLabelPrompt,
		appOwnedContainsSummarizeBranchPrompt: appOwnedSummaries.containsSummarizeBranchPrompt,
		appOwnedContainsBranchSummarizationCancelled: appOwnedSummaries.containsBranchSummarizationCancelled,
		appOwnedContainsBranchFromMessageTitle: appOwnedSummaries.containsBranchFromMessageTitle,
		appOwnedContainsBranchFromMessageSubtitle: appOwnedSummaries.containsBranchFromMessageSubtitle,
		appOwnedContainsDeleteSessionPrompt: appOwnedSummaries.containsDeleteSessionPrompt,
		appOwnedContainsReloadedResources: appOwnedSummaries.containsReloadedResources,
		appOwnedContainsCompactionLabel: appOwnedSummaries.containsCompactionLabel,
		appOwnedContainsBranchLabel: appOwnedSummaries.containsBranchLabel,
		appOwnedContainsSkillLabel: appOwnedSummaries.containsSkillLabel,
		appOwnedContainsCustomDemoLabel: appOwnedSummaries.containsCustomDemoLabel,
		appOwnedContainsCompactedFrom: appOwnedSummaries.containsCompactedFrom,
		appOwnedContainsRetryingStatus: appOwnedSummaries.containsRetryingStatus,
		appOwnedContainsFooterThinkingDetail: appOwnedSummaries.containsFooterThinkingDetail,
		appOwnedContainsFooterModelOnly: appOwnedSummaries.containsFooterModelOnly,
		appOwnedCrashed: appOwnedSummaries.crashed,
		appOwnedShellFallback: appOwnedSummaries.shellFallback,
		...extra,
	};
}

async function runScenario(runtime, scenario, options, branch) {
	const tempRoot = mkdtempSync(join(tmpdir(), `pi-tui-parity-${runtime}-${scenario}-`));
	const session = `pi-${process.pid}-${runtime}-${scenario}`;
	const runtimeRoot = runtimeRootFor(runtime);
	const liveServer = isLiveResponseScenario(scenario) ? await startResponsesServer(scenario) : null;
	const preparedFixtures = prepareScenarioFixtures(tempRoot, scenario, runtimeRoot, liveServer?.baseUrl);
	const sessionPath =
		preparedFixtures.sessionPath ??
		(["read", "write", "edit"].includes(scenario)
			? createSeededSessionFile(tempRoot, scenario, runtimeRoot)
			: undefined);
	const launcherPath = makeLauncher(
		runtime,
		{
			...options,
			scenario,
			sessionPath,
			extraArgs: preparedFixtures.extraArgs,
			extraEnv: preparedFixtures.extraEnv,
			extraPathEntries: preparedFixtures.extraPathEntries,
		},
		runtimeRoot,
		tempRoot,
	);
	const diffId = randomUUID().slice(0, 8);
	const leftName = `.pi-tui-parity-${diffId}-left.txt`;
	const rightName = `.pi-tui-parity-${diffId}-right.txt`;
	const leftPath = join(runtimeRoot, leftName);
	const rightPath = join(runtimeRoot, rightName);
	writeFileSync(leftPath, "alpha\nline-two\n", "utf8");
	writeFileSync(rightPath, "alpha\nline-three\n", "utf8");

	try {
		tmux("kill-session", "-t", session);
		runCommand("tmux", ["new-session", "-d", "-s", session, "-x", String(options.width), "-y", String(options.height)]);
		sendLiteral(session, launcherPath);
		sendKey(session, "Enter");
		await waitForCapture(session, (text) =>
			scenario === "startup-resume" ||
			scenario === "config-browser" ||
			scenario === "footer-variants" ||
			scenario === "footer-subscription" ||
			scenario === "footer-unknown-context"
				? readyForScenario(runtime, scenario, text)
				: bootstrapReady(runtime, text),
		);

		if (scenario === "config-browser") {
			sendLiteral(session, "local-package");
			await waitForCapture(
				session,
				(text) => text.includes("local-package") && text.includes("[x] resource"),
			);
			const initialCapture = captureScenarioSnapshot(
				session,
				runtime,
				scenario,
				runtimeRoot,
				tempRoot,
				branch,
				{
					phase: "initial",
				},
			);
			sendKey(session, "Space");
			await waitForCapture(session, (text) => text.includes("[ ] resource"));
			const activeCapture = captureScenarioSnapshot(
				session,
				runtime,
				scenario,
				runtimeRoot,
				tempRoot,
				branch,
				{
					phase: "active",
				},
			);
			sendKey(session, "Escape");
			await delay(200);
			sendLiteral(session, launcherPath);
			sendKey(session, "Enter");
			await waitForCapture(session, (text) => readyForScenario(runtime, scenario, text));
			sendLiteral(session, "local-package");
			await waitForCapture(
				session,
				(text) => text.includes("local-package") && text.includes("[ ] resource"),
			);
			const settledCapture = captureScenarioSnapshot(
				session,
				runtime,
				scenario,
				runtimeRoot,
				tempRoot,
				branch,
				{
					phase: "settled",
				},
			);
			return {
				...settledCapture,
				frames: {
					initial: initialCapture,
					active: activeCapture,
					settled: settledCapture,
				},
			};
		} else if (scenario === "slash") {
			sendLiteral(session, "/");
		} else if (scenario === "model") {
			sendLiteral(session, "/model");
			sendKey(session, "Enter");
		} else if (scenario === "resume") {
			sendLiteral(session, "/resume");
			sendKey(session, "Enter");
		} else if (scenario === "settings") {
			sendLiteral(session, "/settings");
			sendKey(session, "Enter");
		} else if (scenario === "startup-resume") {
			// No extra input; the startup picker is the scenario under test.
		} else if (scenario === "shift-tab") {
			sendLiteral(session, "\u001b[Z");
		} else if (scenario === "hidden-thinking") {
			sendLiteral(session, scenarioPromptText(scenario));
			sendKey(session, "Enter");
			await waitForCapture(session, (text) => text.includes("Thinking...") || text.includes("Hidden thinking response"));
		} else if (scenario === "live-streaming-start") {
			sendLiteral(session, scenarioPromptText(scenario));
			sendKey(session, "Enter");
		} else if (scenario === "live-streaming-mid") {
			sendLiteral(session, scenarioPromptText(scenario));
			sendKey(session, "Enter");
		} else if (scenario === "abort-active-run") {
			sendLiteral(session, scenarioPromptText(scenario));
			sendKey(session, "Enter");
			await waitForCapture(session, (text) => text.includes("Aborting live run"));
			sendKey(session, "Escape");
			await delay(250);
		} else if (scenario === "tool-lifecycle") {
			sendLiteral(session, scenarioPromptText(scenario));
			sendKey(session, "Enter");
			await waitForCapture(session, (text) => text.includes("tool-lifecycle complete"));
		} else if (scenario === "fork") {
			sendLiteral(session, "/fork");
			sendKey(session, "Enter");
		} else if (scenario === "scoped-models") {
			sendLiteral(session, "/scoped-models");
			sendKey(session, "Enter");
		} else if (
			scenario === "startup-diagnostics" ||
			scenario === "footer-variants" ||
			scenario === "footer-subscription" ||
			scenario === "footer-unknown-context" ||
			scenario === "custom-messages-and-skills" ||
			scenario === "compaction-and-retry"
		) {
			// No extra input; these scenarios are captured from the seeded session/startup surface.
		} else if (scenario === "resume-populated-management") {
			sendLiteral(session, "/resume");
			sendKey(session, "Enter");
			await waitForCapture(session, (text) => text.includes("Resume Session"));
			sendKey(session, "Tab");
			await waitForCapture(
				session,
				(text) =>
					text.includes("Resume Session (All)") ||
					text.includes("Alpha Session") ||
					text.includes("Beta Session") ||
					text.includes("Gamma Session") ||
					text.includes("No sessions found"),
			);
		} else if (scenario === "tree-navigation") {
			sendLiteral(session, "/tree");
			sendKey(session, "Enter");
			await waitForCapture(session, (text) => text.includes("Session Tree"));
			sendKey(session, "Right");
			sendKey(session, "Left");
		} else if (scenario === "tree-summary") {
			sendLiteral(session, "/tree");
			sendKey(session, "Enter");
			await waitForCapture(session, (text) => text.includes("Session Tree"));
			sendKey(session, "Up");
			sendKey(session, "Enter");
		} else if (scenario === "tree-management") {
			sendLiteral(session, "/tree");
			sendKey(session, "Enter");
			await waitForCapture(session, (text) => text.includes("Session Tree"));
			sendKey(session, "L");
			await waitForCapture(session, (text) => text.includes("Label (empty to remove):"));
		} else if (scenario === "fork-populated") {
			sendLiteral(session, "/fork");
			sendKey(session, "Enter");
		} else if (scenario === "login") {
			sendLiteral(session, "/login");
			sendKey(session, "Enter");
		} else if (scenario === "logout") {
			sendLiteral(session, "/logout");
			sendKey(session, "Enter");
		} else if (scenario === "reload-diagnostics") {
			sendLiteral(session, "/reload");
			sendKey(session, "Enter");
		} else if (scenario === "session") {
			sendLiteral(session, "/session");
			sendKey(session, "Enter");
		} else if (scenario === "changelog") {
			sendLiteral(session, "/changelog");
			sendKey(session, "Enter");
		} else if (scenario === "hotkeys") {
			sendLiteral(session, "/hotkeys");
			sendKey(session, "Enter");
		} else if (scenario === "copy-empty") {
			sendLiteral(session, "/copy");
			sendKey(session, "Enter");
		} else if (scenario === "share-missing-gh") {
			sendLiteral(session, "/share");
			sendKey(session, "Enter");
		} else if (scenario === "share-success") {
			sendLiteral(session, "/share");
			sendKey(session, "Enter");
			await waitForCapture(session, (text) => text.includes("Share URL:"));
		} else if (scenario === "share-cancel") {
			sendLiteral(session, "/share");
			sendKey(session, "Enter");
			await waitForCapture(session, (text) => text.includes("Creating gist..."));
			sendKey(session, "Escape");
			await waitForCapture(session, (text) => text.includes("Share cancelled"));
		} else if (scenario === "excluded-bash") {
			sendLiteral(session, "!!printf hello-from-bash");
			sendKey(session, "Enter");
		} else if (scenario === "manual-bash") {
			sendLiteral(session, "!printf hello-from-bash");
		} else if (scenario === "read" || scenario === "write" || scenario === "edit") {
			// Seeded session scenarios render from the session file without extra input.
		} else if (scenario === "sticky-footer") {
			sendLiteral(session, "!for i in $(seq 1 40); do printf 'line-%02d\\n' \"$i\"; done");
			sendKey(session, "Enter");
		} else if (scenario === "bash") {
			sendLiteral(session, "!printf hello-from-bash");
			sendKey(session, "Enter");
		} else if (scenario === "diff") {
			sendLiteral(session, `!git diff --no-index ${leftName} ${rightName}`);
			sendKey(session, "Enter");
		}

		const liveStreaming = isLiveResponseScenario(scenario);
		if (liveStreaming) {
			if (scenario === "live-streaming-start" || scenario === "live-streaming-mid") {
				await waitForCapture(
					session,
					(text) => text.includes("Working...") || text.includes(scenarioPromptText(scenario)),
				);
			} else {
				await delay(60);
			}
				const initialCapture = captureScenarioSnapshot(
					session,
					runtime,
					scenario,
					runtimeRoot,
					tempRoot,
					branch,
					{
						phase: "initial",
					},
				);
			if (scenario === "live-streaming-start") {
				await waitForCapture(
					session,
					(text) =>
						text.includes("Streaming visible thinking start")
						|| text.includes("Live streaming "),
				);
			} else if (scenario === "live-streaming-mid") {
				await waitForCapture(
					session,
					(text) =>
						text.includes("Streaming visible thinking mid")
						|| text.includes("Live streaming "),
				);
			} else if (scenario === "abort-active-run") {
				await waitForCapture(
					session,
					(text) => text.includes("Start a run we can abort.") || text.includes("Aborting live run"),
				);
			} else if (scenario === "hidden-thinking") {
				await waitForCapture(
					session,
					(text) => text.includes("Thinking...") || text.includes("Hidden thinking response"),
				);
			} else if (scenario === "tool-lifecycle") {
				await waitForCapture(
					session,
					(text) => text.includes("$ printf tool-lifecycle") || text.includes("tool-lifecycle complete"),
				);
			}
				const activeCapture = captureScenarioSnapshot(
					session,
					runtime,
					scenario,
					runtimeRoot,
					tempRoot,
					branch,
					{
						phase: "active",
					},
				);
			await waitForCapture(session, (text) => {
				if (scenario === "live-streaming-start") {
					return runtime === "rust"
						? text.includes("Live streaming start") && text.includes("Response received.")
						: text.includes("Live streaming start") && !text.includes("Working...");
				}
				if (scenario === "live-streaming-mid") {
					return runtime === "rust"
						? text.includes("Live streaming mid") && text.includes("Response received.")
						: text.includes("Live streaming mid") && !text.includes("Working...");
				}
				if (scenario === "hidden-thinking") {
					return runtime === "rust"
						? text.includes("Hidden thinking response") && text.includes("Response received.")
						: text.includes("Hidden thinking response") && !text.includes("Working...");
				}
				return readyForScenario(runtime, scenario, text);
			});
				const settledCapture = captureScenarioSnapshot(
					session,
					runtime,
					scenario,
					runtimeRoot,
					tempRoot,
					branch,
					{
						phase: "settled",
					},
				);
			return {
				...settledCapture,
				frames: {
					initial: initialCapture,
					active: activeCapture,
					settled: settledCapture,
				},
				liveStreamingFrames: {
					initial: initialCapture,
					active: activeCapture,
					settled: settledCapture,
				},
			};
		}

		const finalCapture = await waitForCapture(session, (text) => readyForScenario(runtime, scenario, text));
		return buildCaptureRecord(runtime, scenario, finalCapture, capturePaneAnsi(session), runtimeRoot, tempRoot, branch);
	} finally {
		tmux("kill-session", "-t", session);
		try {
			rmSync(leftPath, { force: true });
			rmSync(rightPath, { force: true });
			rmSync(tempRoot, { recursive: true, force: true });
		} catch {
			// tmux teardown can briefly keep the temp root busy; best-effort cleanup is enough here.
		} finally {
			if (liveServer) {
				await liveServer.close();
			}
		}
	}
}

async function main() {
	const options = parseArgs(process.argv.slice(2));
	if (!haveCommand("tmux")) {
		throw new Error("tmux is required for PTY capture.");
	}
	const runtimes = options.runtime === "both" ? ["ts", "rust"] : [options.runtime];
	if (runtimes.includes("ts")) {
		const tsRepo = requireTsRepoDir();
		const tsxPath = join(tsRepo, "node_modules", "tsx", "dist", "cli.mjs");
		if (!existsSync(tsxPath)) {
			throw new Error(`Missing ${tsxPath}. Run npm install in the TypeScript repo.`);
		}
	}
	if (runtimes.includes("rust") && !existsSync(options.rustBin)) {
		throw new Error(`Missing Rust binary: ${options.rustBin}`);
	}
	const result = {
		meta: {
			width: options.width,
			height: options.height,
			scenarios: options.scenarios,
			rustBin: options.rustBin,
		},
		ts: {},
		rust: {},
	};

	for (const runtime of runtimes) {
		const branch = currentBranch(runtimeRootFor(runtime));
		for (const scenario of options.scenarios) {
			result[runtime][scenario] = await runScenario(runtime, scenario, options, branch);
		}
	}

	process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

main().catch((error) => {
	const message = error instanceof Error ? error.stack || error.message : String(error);
	process.stderr.write(`${message}\n`);
	process.exit(1);
});
