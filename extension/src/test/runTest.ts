import { join } from "node:path";
import { runTests } from "@vscode/test-electron";

async function main(): Promise<void> {
	const extensionDevelopmentPath = join(__dirname, "..", "..");
	const extensionTestsPath = join(__dirname, "suite", "index");

	await runTests({ extensionDevelopmentPath, extensionTestsPath });
}

void main().catch((error: unknown) => {
	console.error(error);
	process.exitCode = 1;
});
