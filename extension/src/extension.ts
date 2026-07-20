import * as path from "path";
import * as fs from "fs";
import { workspace, ExtensionContext, window } from "vscode";
import {
	LanguageClient,
	LanguageClientOptions,
	ServerOptions,
	TransportKind,
} from "vscode-languageclient/node";

let client: LanguageClient;

export async function activate(context: ExtensionContext) {
	// The extension launches the native nymph-lsp server. Prefer the debug
	// build (the F5 "Build extension + LSP server" task produces it); fall back
	// to the release build if only that exists. The same binary is used for
	// both run and debug so the client can never pick a path that wasn't built.
	const debugModule = context.asAbsolutePath(path.join("..", "target", "debug", "nymph-lsp"));
	const releaseModule = context.asAbsolutePath(path.join("..", "target", "release", "nymph-lsp"));
	const server = fs.existsSync(debugModule) ? debugModule : releaseModule;

	if (!fs.existsSync(server)) {
		window.showErrorMessage(
			'nymph-lsp binary not found. Build it with `cargo build -p nymph-lsp` (or run the "Build extension + LSP server" task), then reload the window.',
		);
		return;
	}

	const serverOptions: ServerOptions = {
		run: { command: server, transport: TransportKind.stdio },
		debug: { command: server, transport: TransportKind.stdio },
	};

	const clientOptions: LanguageClientOptions = {
		documentSelector: [{ scheme: "file", language: "nymph" }],
		synchronize: {
			fileEvents: workspace.createFileSystemWatcher("**/*.nym"),
		},
	};

	client = new LanguageClient("nymph-lsp", "Nymph Language Server", serverOptions, clientOptions);

	try {
		await client.start();
		window.showInformationMessage("Nymph Language Server activated");
	} catch (err) {
		window.showErrorMessage(`Failed to start Nymph Language Server: ${err}`);
	}
}

export async function deactivate(): Promise<void> {
	if (!client) {
		return undefined;
	}
	return client.stop();
}
