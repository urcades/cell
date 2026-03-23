#!/usr/bin/env node

import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
	createAssistantMessageEventStream,
	registerApiProvider,
	type AssistantMessage,
	type Context,
	type Model,
	type SimpleStreamOptions,
	type TextContent,
	type Usage,
	type UserMessage,
} from "../../packages/ai/src/index.js";
import {
	AuthStorage,
	ModelRegistry,
	SessionManager,
	createAgentSession,
	main as runCliMain,
	runPrintMode,
	runRpcMode,
} from "../../packages/coding-agent/src/index.js";
import { exportFromFile } from "../../packages/coding-agent/src/core/export-html/index.js";

type Scenario =
	| "print-text"
	| "print-json"
	| "rpc"
	| "rpc-child"
	| "session-artifact"
	| "resource-precedence"
	| "package-commands"
	| "rpc-images"
	| "rpc-bash"
	| "export-cli";

interface FixtureEnvironment {
	rootDir: string;
	cwd: string;
	agentDir: string;
	sessionDir: string;
	sessionManager: SessionManager;
	session: Awaited<ReturnType<typeof createAgentSession>>["session"];
}

function resolveTsRepoDir() {
	const tsRepo = process.env.PI_TS_REPO?.trim();
	if (!tsRepo) {
		throw new Error("PI_TS_REPO is required for TS parity capture. Set it to the TypeScript repo root.");
	}
	const resolved = resolve(tsRepo);
	if (!existsSync(resolved)) {
		throw new Error(`PI_TS_REPO does not exist: ${resolved}`);
	}
	return resolved;
}

const TS_REPO_DIR = resolveTsRepoDir();
const TSX_CLI_PATH = join(TS_REPO_DIR, "node_modules", "tsx", "dist", "cli.mjs");

Date.now = () => 0;

const usage: Usage = {
	input: 1,
	output: 1,
	cacheRead: 0,
	cacheWrite: 0,
	totalTokens: 2,
	cost: {
		input: 0,
		output: 0,
		cacheRead: 0,
		cacheWrite: 0,
		total: 0,
	},
};

registerApiProvider({
	api: "openai-responses",
	stream: (model, context, options) => createMockStream(model, context, options),
	streamSimple: (model, context, options) => createMockStream(model, context, options),
});

function createMockStream(
	model: Model<any>,
	context: Context,
	_options?: SimpleStreamOptions,
) {
	const stream = createAssistantMessageEventStream();
	const prompt = extractPromptText(context.messages[context.messages.length - 1]);
	const assistant = createAssistantMessage(model, `echo:${prompt}`);
	queueMicrotask(() => {
		stream.push({
			type: "done",
			reason: "stop",
			message: assistant,
		});
	});
	return stream;
}

function createAssistantMessage(model: Model<any>, text: string): AssistantMessage {
	return {
		role: "assistant",
		content: [
			{
				type: "text",
				text,
			} satisfies TextContent,
		],
		api: model.api,
		provider: model.provider,
		model: model.id,
		usage,
		stopReason: "stop",
		timestamp: 0,
	};
}

function extractPromptText(message: UserMessage | AssistantMessage | undefined): string {
	if (!message || message.role !== "user") {
		return "";
	}
	if (typeof message.content === "string") {
		return message.content;
	}
	return message.content
		.filter((part): part is TextContent => part.type === "text")
		.map((part) => part.text)
		.join("");
}

async function withFixtureEnvironment(
	options: { persistentSession?: boolean },
	fn: (env: FixtureEnvironment) => Promise<unknown>,
): Promise<unknown> {
	const rootDir = mkdtempSync(join(tmpdir(), "pi-ts-parity-"));
	const cwd = join(rootDir, "workspace", "app");
	const agentDir = join(rootDir, "agent");
	const sessionDir = join(rootDir, "sessions");
	mkdirSync(cwd, { recursive: true });
	mkdirSync(agentDir, { recursive: true });
	if (options.persistentSession) {
		mkdirSync(sessionDir, { recursive: true });
	}

	const authStorage = AuthStorage.inMemory();
	authStorage.setRuntimeApiKey("openai", "fixture-key");
	const modelRegistry = new ModelRegistry(authStorage, undefined);
	const model = modelRegistry.find("openai", "gpt-5.1-codex");
	if (!model) {
		throw new Error("Built-in openai/gpt-5.1-codex model not found");
	}

	const sessionManager = options.persistentSession
		? SessionManager.create(cwd, sessionDir)
		: SessionManager.inMemory(cwd);
	const { session } = await createAgentSession({
		cwd,
		agentDir,
		authStorage,
		modelRegistry,
		model,
		thinkingLevel: "off",
		sessionManager,
	});

	try {
		return await fn({ rootDir, cwd, agentDir, sessionDir, sessionManager, session });
	} finally {
		rmSync(rootDir, { recursive: true, force: true });
	}
}

async function captureConsole<T>(fn: () => Promise<T>): Promise<{ logs: string[]; errors: string[]; result: T }> {
	const logs: string[] = [];
	const errors: string[] = [];
	const originalLog = console.log;
	const originalError = console.error;
	console.log = (...args: unknown[]) => {
		logs.push(args.map(String).join(" "));
	};
	console.error = (...args: unknown[]) => {
		errors.push(args.map(String).join(" "));
	};
	try {
		const result = await fn();
		return { logs, errors, result };
	} finally {
		console.log = originalLog;
		console.error = originalError;
	}
}

function writeFile(path: string, content: string): void {
	mkdirSync(dirname(path), { recursive: true });
	writeFileSync(path, content, "utf8");
}

async function runCliCommand(
	env: Pick<FixtureEnvironment, "cwd" | "agentDir">,
	args: string[],
): Promise<{ stdoutLines: string[]; stderrLines: string[]; exitCode: number }> {
	const originalCwd = process.cwd();
	const originalAgentDir = process.env.PI_CODING_AGENT_DIR;
	const originalExitCode = process.exitCode;
	const originalNpmLoglevel = process.env.npm_config_loglevel;
	const originalNpmAudit = process.env.npm_config_audit;
	const originalNpmFund = process.env.npm_config_fund;
	const originalStdoutWrite = process.stdout.write.bind(process.stdout);
	const originalStderrWrite = process.stderr.write.bind(process.stderr);
	let stdoutBuffer = "";
	let stderrBuffer = "";

	process.chdir(env.cwd);
	process.env.PI_CODING_AGENT_DIR = env.agentDir;
	process.env.npm_config_loglevel = "silent";
	process.env.npm_config_audit = "false";
	process.env.npm_config_fund = "false";
	process.exitCode = undefined;
	(process.stdout.write as typeof process.stdout.write) = ((chunk: any, ...rest: any[]) => {
		stdoutBuffer += String(chunk);
		const callback = rest.find((value) => typeof value === "function");
		if (callback) callback();
		return true;
	}) as typeof process.stdout.write;
	(process.stderr.write as typeof process.stderr.write) = ((chunk: any, ...rest: any[]) => {
		stderrBuffer += String(chunk);
		const callback = rest.find((value) => typeof value === "function");
		if (callback) callback();
		return true;
	}) as typeof process.stderr.write;

	try {
		await runCliMain(args);
		return {
			stdoutLines: stdoutBuffer.split("\n").filter(Boolean),
			stderrLines: stderrBuffer.split("\n").filter(Boolean),
			exitCode: process.exitCode ?? 0,
		};
	} finally {
		process.stdout.write = originalStdoutWrite;
		process.stderr.write = originalStderrWrite;
		process.chdir(originalCwd);
		if (originalAgentDir === undefined) {
			delete process.env.PI_CODING_AGENT_DIR;
		} else {
			process.env.PI_CODING_AGENT_DIR = originalAgentDir;
		}
		if (originalNpmLoglevel === undefined) {
			delete process.env.npm_config_loglevel;
		} else {
			process.env.npm_config_loglevel = originalNpmLoglevel;
		}
		if (originalNpmAudit === undefined) {
			delete process.env.npm_config_audit;
		} else {
			process.env.npm_config_audit = originalNpmAudit;
		}
		if (originalNpmFund === undefined) {
			delete process.env.npm_config_fund;
		} else {
			process.env.npm_config_fund = originalNpmFund;
		}
		process.exitCode = originalExitCode;
	}
}

function normalizePath(value: string, env: Pick<FixtureEnvironment, "rootDir" | "cwd" | "agentDir" | "sessionDir">): string {
	return value
		.replaceAll(`/private${env.cwd}`, "<CWD>")
		.replaceAll(`/private${env.agentDir}`, "<AGENT_DIR>")
		.replaceAll(`/private${env.sessionDir}`, "<SESSION_DIR>")
		.replaceAll(`/private${env.rootDir}`, "<TMP>")
		.replaceAll(env.cwd, "<CWD>")
		.replaceAll(env.agentDir, "<AGENT_DIR>")
		.replaceAll(env.sessionDir, "<SESSION_DIR>")
		.replaceAll(env.rootDir, "<TMP>");
}

function normalizeSystemPromptText(
	value: string,
	env: Pick<FixtureEnvironment, "rootDir" | "cwd" | "agentDir" | "sessionDir">,
): string {
	return normalizePath(value, env).replace(
		/Current date and time: .+/,
		"Current date and time: <NOW>",
	);
}

function normalizeJsonValue(
	value: unknown,
	env: Pick<FixtureEnvironment, "rootDir" | "cwd" | "agentDir" | "sessionDir">,
): unknown {
	if (Array.isArray(value)) {
		return value.map((item) => normalizeJsonValue(item, env));
	}
	if (!value || typeof value !== "object") {
		if (typeof value === "string") {
			return normalizePath(value, env);
		}
		return value;
	}

	const record = value as Record<string, unknown>;
	const normalized: Record<string, unknown> = {};
	for (const [key, item] of Object.entries(record)) {
		if (key === "timestamp") {
			normalized[key] = 0;
			continue;
		}
		if (key === "cwd") {
			normalized[key] = "<CWD>";
			continue;
		}
		if (key === "sessionId") {
			normalized[key] = "<SESSION_ID>";
			continue;
		}
		if (key === "id" && record.type === "session") {
			normalized[key] = "<SESSION_ID>";
			continue;
		}
		normalized[key] = normalizeJsonValue(item, env);
	}
	return normalized;
}

function normalizeSessionArtifact(
	entries: Array<Record<string, any>>,
	env: Pick<FixtureEnvironment, "rootDir" | "cwd" | "agentDir" | "sessionDir">,
) {
	return entries.map((entry) => {
		switch (entry.type) {
			case "session":
				return {
					type: "session",
					cwd: "<CWD>",
				};
			case "model_change":
				return {
					type: "model_change",
					provider: entry.provider,
					modelId: entry.modelId,
				};
			case "thinking_level_change":
				return {
					type: "thinking_level_change",
					thinkingLevel: entry.thinkingLevel ?? entry.level,
				};
			case "message":
				return summarizeMessageEntry(entry.message, env);
			default:
				return normalizeJsonValue(entry, env);
		}
	});
}

function summarizeMessageEntry(
	message: Record<string, any>,
	env: Pick<FixtureEnvironment, "rootDir" | "cwd" | "agentDir" | "sessionDir">,
) {
	if (message.role === "user") {
		return {
			type: "message",
			role: "user",
			text: extractTextBlocks(message.content),
		};
	}
	if (message.role === "assistant") {
		return {
			type: "message",
			role: "assistant",
			stopReason: message.stopReason,
			text: extractAssistantTextBlocks(message.content),
		};
	}
	if (message.role === "toolResult") {
		return {
			type: "message",
			role: "toolResult",
			toolName: message.toolName,
			isError: !!message.isError,
			text: extractTextBlocks(message.content),
		};
	}
	return normalizeJsonValue(message, env);
}

function extractTextBlocks(content: unknown): string {
	if (typeof content === "string") {
		return content;
	}
	if (!Array.isArray(content)) {
		return "";
	}
	return content
		.filter((part): part is { type: string; text?: string } => !!part && typeof part === "object")
		.filter((part) => part.type === "text")
		.map((part) => part.text ?? "")
		.join("");
}

function extractAssistantTextBlocks(content: unknown): string {
	if (!Array.isArray(content)) {
		return "";
	}
	return content
		.filter((part): part is { type: string; text?: string } => !!part && typeof part === "object")
		.filter((part) => part.type === "text")
		.map((part) => part.text ?? "")
		.join("");
}

async function capturePrintText() {
	return withFixtureEnvironment({}, async (env) => {
		const { logs, errors } = await captureConsole(async () => {
			await runPrintMode(env.session, {
				mode: "text",
				initialMessage: "hello",
			});
		});
		return {
			stdoutLines: logs,
			stderrLines: errors,
		};
	});
}

async function capturePrintJson() {
	return withFixtureEnvironment({}, async (env) => {
		const { logs, errors } = await captureConsole(async () => {
			await runPrintMode(env.session, {
				mode: "json",
				initialMessage: "hello",
			});
		});
		return {
			lines: logs.map((line) => normalizeJsonValue(JSON.parse(line), env)),
			stderrLines: errors,
		};
	});
}

async function captureRpcTranscript() {
	return withFixtureEnvironment({}, async (env) => {
		const child = spawn(
			process.execPath,
			[
				TSX_CLI_PATH,
				fileURLToPath(import.meta.url),
				"rpc-child",
			],
			{
				cwd: TS_REPO_DIR,
				stdio: ["pipe", "pipe", "pipe"],
			},
		);

		const stdoutChunks: string[] = [];
		const stderrChunks: string[] = [];
		let parsedLines: Array<Record<string, unknown>> = [];
		let resolved = false;

		child.stdout.setEncoding("utf8");
		child.stdout.on("data", (chunk: string) => {
			stdoutChunks.push(chunk);
			parsedLines = stdoutChunks
				.join("")
				.split("\n")
				.filter(Boolean)
				.map((line) => JSON.parse(line));
		});
		child.stderr.setEncoding("utf8");
		child.stderr.on("data", (chunk: string) => {
			stderrChunks.push(chunk);
		});

		const waitForOutput = async (
			label: string,
			predicate: (lines: Array<Record<string, unknown>>) => boolean,
		): Promise<Array<Record<string, unknown>>> => {
			for (let attempt = 0; attempt < 200; attempt += 1) {
				if (predicate(parsedLines)) {
					return parsedLines;
				}
				await new Promise((resolve) => setTimeout(resolve, 10));
			}
			throw new Error(`Timed out waiting for RPC ${label}\n${stderrChunks.join("")}`);
		};

		const exitPromise = new Promise<void>((resolve, reject) => {
			child.on("error", reject);
			child.on("exit", (code) => {
				if (resolved || code === 0 || code === null) {
					resolve();
					return;
				}
				reject(new Error(`RPC child exited before transcript completed: ${code}\n${stderrChunks.join("")}`));
			});
		});

		child.stdin.write(JSON.stringify({ type: "get_state", id: "1" }) + "\n");
		await waitForOutput(
			"get_state response",
			(lines) => lines.some((line) => line.type === "response" && line.command === "get_state"),
		);

		child.stdin.write(JSON.stringify({ type: "prompt", id: "2", message: "hello" }) + "\n");
		await waitForOutput(
			"agent_end event",
			(lines) => lines.some((line) => line.type === "agent_end"),
		);

		child.stdin.write(JSON.stringify({ type: "get_last_assistant_text", id: "3" }) + "\n");
		const lines = await waitForOutput(
			"get_last_assistant_text response",
			(output) =>
				output.some(
					(line) => line.type === "response" && line.command === "get_last_assistant_text",
				),
		);
		resolved = true;
		child.kill("SIGTERM");
		await exitPromise;
		return {
			lines: lines.map((line) => normalizeJsonValue(line, env)),
			stderrLines: stderrChunks.join("").split("\n").filter(Boolean),
		};
	});
}

async function captureSessionArtifact() {
	return withFixtureEnvironment({ persistentSession: true }, async (env) => {
		await captureConsole(async () => {
			await runPrintMode(env.session, {
				mode: "text",
				initialMessage: "hello",
			});
		});
		const sessionFile = env.sessionManager.getSessionFile();
		if (!sessionFile) {
			throw new Error("Expected persistent session file");
		}
		const lines = readFileSync(sessionFile, "utf8")
			.split("\n")
			.filter((line) => line.trim().length > 0);
		const entries = lines.map((line) => JSON.parse(line));
		return {
			entries: normalizeSessionArtifact(entries, env),
		};
	});
}

async function captureResourcePrecedence() {
	return withFixtureEnvironment({}, async (env) => {
		writeFile(join(env.agentDir, "SYSTEM.md"), "global system");
		writeFile(join(dirname(dirname(env.cwd)), "AGENTS.md"), "root instructions");
		writeFile(join(env.cwd, "AGENTS.md"), "cwd instructions");
		writeFile(join(env.cwd, ".pi", "APPEND_SYSTEM.md"), "project append");
		writeFile(
			join(env.cwd, ".pi", "prompts", "review.md"),
			"---\ndescription: Review a target\n---\nReview $1 with $2",
		);
		writeFile(
			join(env.cwd, ".pi", "skills", "checks", "SKILL.md"),
			"---\nname: checks\ndescription: Run verification checks\n---\nUse the checks skill.",
		);

		const authStorage = AuthStorage.inMemory();
		authStorage.setRuntimeApiKey("openai", "fixture-key");
		const modelRegistry = new ModelRegistry(authStorage, undefined);
		const model = modelRegistry.find("openai", "gpt-5.1-codex");
		if (!model) {
			throw new Error("Built-in openai/gpt-5.1-codex model not found");
		}
		const sessionManager = SessionManager.inMemory(env.cwd);
		const { session } = await createAgentSession({
			cwd: env.cwd,
			agentDir: env.agentDir,
			authStorage,
			modelRegistry,
			model,
			thinkingLevel: "off",
			sessionManager,
		});

		const resourceLoader = (session as any)._resourceLoader as DefaultResourceLoader;
		const commands = [
			...session.promptTemplates.map((template) => ({
				name: template.name,
				description: template.description,
				source: "prompt",
				location: template.source,
				path: normalizePath(template.filePath, env),
			})),
			...resourceLoader.getSkills().skills.map((skill) => ({
				name: `skill:${skill.name}`,
				description: skill.description,
				source: "skill",
				location: skill.source,
				path: normalizePath(skill.filePath, env),
			})),
		];

		return {
			commands,
			systemPrompt: normalizeSystemPromptText(session.systemPrompt, env),
		};
	});
}

async function capturePackageCommands() {
	return withFixtureEnvironment({}, async (env) => {
		writeFile(join(env.cwd, "pkg", "README.md"), "local package");
		writeFile(join(env.cwd, "npm-pkg", "package.json"), '{"name":"fixture-pkg","version":"1.0.0"}');
		writeFile(join(env.cwd, "npm-pkg", "index.js"), "module.exports = 1;\n");

		const installLocal = await runCliCommand(env, ["install", "./pkg", "--local"]);
		const list = await runCliCommand(env, ["list"]);
		const removeLocal = await runCliCommand(env, ["remove", "./pkg", "--local"]);
		const installNpm = await runCliCommand(env, ["install", "npm:./npm-pkg", "--local"]);
		const updateNpm = await runCliCommand(env, ["update", "npm:./npm-pkg"]);
		const removeNpm = await runCliCommand(env, ["remove", "npm:./npm-pkg", "--local"]);

		const normalizeCapture = (capture: { stdoutLines: string[]; stderrLines: string[]; exitCode: number }) => ({
			exitCode: capture.exitCode,
			stdoutLines: capture.stdoutLines.map((line) => normalizePath(line, env)),
			stderrLines: capture.stderrLines.map((line) => normalizePath(line, env)),
		});

		return {
			installLocal: normalizeCapture(installLocal),
			installNpm: normalizeCapture(installNpm),
			list: normalizeCapture(list),
			updateNpm: normalizeCapture(updateNpm),
			removeNpm: normalizeCapture(removeNpm),
			removeLocal: normalizeCapture(removeLocal),
		};
	});
}

async function captureRpcImages() {
	return withFixtureEnvironment({}, async (env) => {
		const child = spawn(
			process.execPath,
			[TSX_CLI_PATH, fileURLToPath(import.meta.url), "rpc-child"],
			{
				cwd: TS_REPO_DIR,
				stdio: ["pipe", "pipe", "pipe"],
			},
		);

		const stdoutChunks: string[] = [];
		const stderrChunks: string[] = [];
		let parsedLines: Array<Record<string, unknown>> = [];
		let resolved = false;

		child.stdout.setEncoding("utf8");
		child.stdout.on("data", (chunk: string) => {
			stdoutChunks.push(chunk);
			parsedLines = stdoutChunks
				.join("")
				.split("\n")
				.filter(Boolean)
				.map((line) => JSON.parse(line));
		});
		child.stderr.setEncoding("utf8");
		child.stderr.on("data", (chunk: string) => {
			stderrChunks.push(chunk);
		});

		const waitForOutput = async (
			label: string,
			predicate: (lines: Array<Record<string, unknown>>) => boolean,
		): Promise<Array<Record<string, unknown>>> => {
			for (let attempt = 0; attempt < 200; attempt += 1) {
				if (predicate(parsedLines)) {
					return parsedLines;
				}
				await new Promise((resolve) => setTimeout(resolve, 10));
			}
			throw new Error(`Timed out waiting for RPC ${label}\n${stderrChunks.join("")}`);
		};

		const exitPromise = new Promise<void>((resolve, reject) => {
			child.on("error", reject);
			child.on("exit", (code) => {
				if (resolved || code === 0 || code === null) {
					resolve();
					return;
				}
				reject(new Error(`RPC child exited before transcript completed: ${code}\n${stderrChunks.join("")}`));
			});
		});

		child.stdin.write(
			JSON.stringify({
				type: "prompt",
				id: "1",
				message: "see image",
				images: [{ type: "image", data: "ZmFrZQ==", mimeType: "image/png" }],
			}) + "\n",
		);
		await waitForOutput("agent_end event", (lines) => lines.some((line) => line.type === "agent_end"));
		child.stdin.write(JSON.stringify({ type: "get_messages", id: "2" }) + "\n");
		const lines = await waitForOutput(
			"get_messages response",
			(output) => output.some((line) => line.type === "response" && line.command === "get_messages"),
		);
		resolved = true;
		child.kill("SIGTERM");
		await exitPromise;

		return {
			lines: lines.map((line) => normalizeJsonValue(line, env)),
			stderrLines: stderrChunks.join("").split("\n").filter(Boolean),
		};
	});
}

async function captureRpcBash() {
	return withFixtureEnvironment({}, async (env) => {
		const child = spawn(
			process.execPath,
			[TSX_CLI_PATH, fileURLToPath(import.meta.url), "rpc-child"],
			{
				cwd: TS_REPO_DIR,
				stdio: ["pipe", "pipe", "pipe"],
			},
		);

		const stdoutChunks: string[] = [];
		const stderrChunks: string[] = [];
		let parsedLines: Array<Record<string, unknown>> = [];
		let resolved = false;

		child.stdout.setEncoding("utf8");
		child.stdout.on("data", (chunk: string) => {
			stdoutChunks.push(chunk);
			parsedLines = stdoutChunks
				.join("")
				.split("\n")
				.filter(Boolean)
				.map((line) => JSON.parse(line));
		});
		child.stderr.setEncoding("utf8");
		child.stderr.on("data", (chunk: string) => {
			stderrChunks.push(chunk);
		});

		const waitForOutput = async (): Promise<Array<Record<string, unknown>>> => {
			for (let attempt = 0; attempt < 200; attempt += 1) {
				if (parsedLines.some((line) => line.type === "response" && line.command === "bash")) {
					return parsedLines;
				}
				await new Promise((resolve) => setTimeout(resolve, 10));
			}
			throw new Error(`Timed out waiting for bash response\n${stderrChunks.join("")}`);
		};

		const exitPromise = new Promise<void>((resolve, reject) => {
			child.on("error", reject);
			child.on("exit", (code) => {
				if (resolved || code === 0 || code === null) {
					resolve();
					return;
				}
				reject(new Error(`RPC child exited before bash transcript completed: ${code}\n${stderrChunks.join("")}`));
			});
		});

		child.stdin.write(JSON.stringify({ type: "bash", id: "1", command: "printf 'hello'" }) + "\n");
		const lines = await waitForOutput();
		resolved = true;
		child.kill("SIGTERM");
		await exitPromise;

		return {
			lines: lines.map((line) => normalizeJsonValue(line, env)),
			stderrLines: stderrChunks.join("").split("\n").filter(Boolean),
		};
	});
}

async function captureExportCli() {
	return withFixtureEnvironment({ persistentSession: true }, async (env) => {
		await captureConsole(async () => {
			await runPrintMode(env.session, {
				mode: "text",
				initialMessage: "hello",
			});
		});
		const sessionFile = env.sessionManager.getSessionFile();
		if (!sessionFile) {
			throw new Error("Expected persistent session file");
		}
		const outputPath = join(env.rootDir, "export.html");
		const exportedPath = await exportFromFile(sessionFile, outputPath);
		const html = readFileSync(outputPath, "utf8");
		return {
			exitCode: 0,
			stdoutLines: [normalizePath(exportedPath, env)],
			stderrLines: [],
			htmlChecks: {
				containsAssistant: html.includes("assistant"),
			},
		};
	});
}

async function runRpcChild() {
	await withFixtureEnvironment({}, async (env) => {
		await runRpcMode(env.session);
	});
}

async function main() {
	const scenario = process.argv[2] as Scenario | undefined;
	switch (scenario) {
		case "print-text":
			process.stdout.write(JSON.stringify(await capturePrintText(), null, 2));
			return;
		case "print-json":
			process.stdout.write(JSON.stringify(await capturePrintJson(), null, 2));
			return;
		case "rpc":
			process.stdout.write(JSON.stringify(await captureRpcTranscript(), null, 2));
			return;
		case "session-artifact":
			process.stdout.write(JSON.stringify(await captureSessionArtifact(), null, 2));
			return;
		case "resource-precedence":
			process.stdout.write(JSON.stringify(await captureResourcePrecedence(), null, 2));
			return;
		case "package-commands":
			process.stdout.write(JSON.stringify(await capturePackageCommands(), null, 2));
			return;
		case "rpc-images":
			process.stdout.write(JSON.stringify(await captureRpcImages(), null, 2));
			return;
		case "rpc-bash":
			process.stdout.write(JSON.stringify(await captureRpcBash(), null, 2));
			return;
		case "export-cli":
			process.stdout.write(JSON.stringify(await captureExportCli(), null, 2));
			return;
		case "rpc-child":
			await runRpcChild();
			return;
		default:
			throw new Error(`Unknown scenario: ${scenario ?? "<missing>"}`);
	}
}

main().catch((error) => {
	console.error(error instanceof Error ? error.stack ?? error.message : String(error));
	process.exit(1);
});
