import * as path from "path";
import { workspace, ExtensionContext, window } from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";

let client: LanguageClient;

export async function activate(context: ExtensionContext) {
  const serverModule = context.asAbsolutePath(
    path.join("..", "target", "release", "nymph-lsp"),
  );

  // Try debug build if release doesn't exist
  const debugModule = context.asAbsolutePath(
    path.join("..", "target", "debug", "nymph-lsp"),
  );

  const serverOptions: ServerOptions = {
    run: { command: serverModule, transport: TransportKind.stdio },
    debug: { command: debugModule, transport: TransportKind.stdio },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "nymph" }],
    synchronize: {
      fileEvents: workspace.createFileSystemWatcher("**/*.nym"),
    },
  };

  client = new LanguageClient(
    "nymph-lsp",
    "Nymph Language Server",
    serverOptions,
    clientOptions,
  );

  try {
    await client.start();
    window.showInformationMessage("Nymph Language Server activated");
  } catch (err) {
    window.showErrorMessage(`Failed to start Nymph Language Server: ${err}`);
    throw err;
  }
}

export async function deactivate(): Promise<void> {
  if (!client) {
    return undefined;
  }
  return client.stop();
}
