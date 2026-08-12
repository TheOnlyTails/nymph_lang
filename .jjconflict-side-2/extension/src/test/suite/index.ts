import assert from "node:assert/strict";
import * as vscode from "vscode";

async function setSelection(
	editor: vscode.TextEditor,
	start: vscode.Position,
	end: vscode.Position,
) {
	editor.selection = new vscode.Selection(start, end);
	await vscode.commands.executeCommand("editor.action.blockComment");
}

export async function run(): Promise<void> {
	const document = await vscode.workspace.openTextDocument({
		language: "nymph",
		content: "let single = 1\nlet first = 2\nlet second = 3\n",
	});
	const editor = await vscode.window.showTextDocument(document);
	const extension = vscode.extensions.getExtension(["theonlytails", "nymph"].join("."));
	assert.ok(extension, "Nymph extension is installed in the smoke-test host");
	await extension.activate();
	const hovers = await vscode.commands.executeCommand<vscode.Hover[]>(
		"vscode.executeHoverProvider",
		document.uri,
		new vscode.Position(0, 13),
	);
	assert.ok(hovers.length > 0, "untitled Nymph documents receive language-server features");

	await setSelection(editor, new vscode.Position(0, 0), new vscode.Position(0, 14));
	assert.equal(document.lineAt(0).text, "/* let single = 1 */");
	await setSelection(editor, new vscode.Position(0, 0), new vscode.Position(0, 20));
	assert.equal(document.lineAt(0).text, "let single = 1");

	await setSelection(editor, new vscode.Position(1, 0), new vscode.Position(2, 14));
	assert.equal(document.getText(), "let single = 1\n/* let first = 2\nlet second = 3 */\n");
	await setSelection(editor, new vscode.Position(1, 0), new vscode.Position(2, 17));
	assert.equal(document.getText(), "let single = 1\nlet first = 2\nlet second = 3\n");
}
