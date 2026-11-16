import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import * as vscode from 'vscode';
import {
    LanguageClient,
    LanguageClientOptions,
    ServerOptions,
    Executable
} from 'vscode-languageclient/node';

// The guide is not served from byard.dev yet; its editor section on GitHub is.
const GUIDE_URL = 'https://github.com/Briany4717/byard/blob/main/docs/guide/src/dev-loop.md#in-your-editor';
const SERVER_NAME = process.platform === 'win32' ? 'byld-lsp.exe' : 'byld-lsp';

let client: LanguageClient | undefined;
// The "not found" notification is shown once per window; the restart command
// is an explicit request, so it always shows it again.
let notifiedMissing = false;

function isFile(p: string): boolean {
    try {
        return fs.statSync(p).isFile();
    } catch {
        return false;
    }
}

// The places `byld-lsp` can be, in order: the setting, PATH, cargo's bin
// directory (a GUI-launched VS Code often lacks it in PATH), then the build
// directories of a checkout of the repository.
function findServer(context: vscode.ExtensionContext): string | undefined {
    const configured = vscode.workspace
        .getConfiguration('byld')
        .get<string | null>('lsp.serverPath');
    if (configured && configured.trim() !== '') {
        // An explicit setting is never second-guessed by falling through.
        return isFile(configured) ? configured : undefined;
    }

    const candidates: string[] = [];
    for (const dir of (process.env.PATH || '').split(path.delimiter)) {
        if (dir) {
            candidates.push(path.join(dir, SERVER_NAME));
        }
    }
    const cargoHome = process.env.CARGO_HOME || path.join(os.homedir(), '.cargo');
    candidates.push(path.join(cargoHome, 'bin', SERVER_NAME));
    const roots: string[] = [path.join(context.extensionPath, '..', '..')];
    for (const folder of vscode.workspace.workspaceFolders || []) {
        roots.push(folder.uri.fsPath);
    }
    for (const root of roots) {
        candidates.push(path.join(root, 'target', 'release', SERVER_NAME));
        candidates.push(path.join(root, 'target', 'debug', SERVER_NAME));
    }

    return candidates.find(isFile);
}

async function showMissing(context: vscode.ExtensionContext): Promise<void> {
    const install = 'Install with cargo';
    const setPath = 'Set path';
    const learn = 'Learn more';
    const choice = await vscode.window.showWarningMessage(
        'Byld: the language server `byld-lsp` was not found',
        install,
        setPath,
        learn
    );
    if (choice === install) {
        // A task, not a bare terminal: its end is an event with an exit code,
        // so the server starts by itself once cargo has built it.
        const task = new vscode.Task(
            { type: 'shell', task: 'install byld-lsp' },
            vscode.TaskScope.Global,
            'install byld-lsp',
            'byld',
            new vscode.ShellExecution('cargo install byld-lsp')
        );
        const execution = await vscode.tasks.executeTask(task);
        const done = vscode.tasks.onDidEndTaskProcess(async (e) => {
            if (e.execution !== execution) {
                return;
            }
            done.dispose();
            if (e.exitCode === 0) {
                await restartServer(context);
            } else {
                void vscode.window.showErrorMessage(
                    'Byld: `cargo install byld-lsp` failed; its output is in the terminal.'
                );
            }
        });
        context.subscriptions.push(done);
    } else if (choice === setPath) {
        await vscode.commands.executeCommand('workbench.action.openSettings', 'byld.lsp.serverPath');
    } else if (choice === learn) {
        await vscode.env.openExternal(vscode.Uri.parse(GUIDE_URL));
    }
}

async function startServer(context: vscode.ExtensionContext, explicit: boolean): Promise<void> {
    const command = findServer(context);
    if (!command) {
        if (explicit || !notifiedMissing) {
            notifiedMissing = true;
            // Not awaited: the notification waits for a click, startup must not.
            void showMissing(context);
        }
        return;
    }

    const run: Executable = {
        command,
        options: {
            env: {
                ...process.env,
                RUST_BACKTRACE: '1',
            }
        }
    };
    const serverOptions: ServerOptions = { run, debug: run };
    const clientOptions: LanguageClientOptions = {
        documentSelector: [{ scheme: 'file', language: 'byld' }],
        synchronize: {
            fileEvents: vscode.workspace.createFileSystemWatcher('**/*.byd')
        }
    };

    const created = new LanguageClient('byldLsp', 'Byld Language Server', serverOptions, clientOptions);
    client = created;
    try {
        await created.start();
    } catch (err) {
        client = undefined;
        void vscode.window.showErrorMessage(`Byld: the language server failed to start (${String(err)})`);
    }
}

async function stopServer(): Promise<void> {
    const current = client;
    client = undefined;
    if (current) {
        try {
            await current.stop();
        } catch {
            // Already stopped or never started: nothing to clean up.
        }
    }
}

async function restartServer(context: vscode.ExtensionContext): Promise<void> {
    await stopServer();
    await startServer(context, true);
}

function colorProvider(): vscode.DocumentColorProvider {
    return {
        provideDocumentColors(document) {
            const colors: vscode.ColorInformation[] = [];
            const text = document.getText();
            const regex = /\b0x([0-9a-fA-F]{6}|[0-9a-fA-F]{8})\b/g;
            let match;
            while ((match = regex.exec(text)) !== null) {
                const hex = match[1];
                const startPos = document.positionAt(match.index);
                const endPos = document.positionAt(match.index + match[0].length);
                const range = new vscode.Range(startPos, endPos);

                let red = 0, green = 0, blue = 0, alpha = 1.0;
                if (hex.length === 6) {
                    red = parseInt(hex.slice(0, 2), 16) / 255;
                    green = parseInt(hex.slice(2, 4), 16) / 255;
                    blue = parseInt(hex.slice(4, 6), 16) / 255;
                } else if (hex.length === 8) {
                    alpha = parseInt(hex.slice(0, 2), 16) / 255;
                    red = parseInt(hex.slice(2, 4), 16) / 255;
                    green = parseInt(hex.slice(4, 6), 16) / 255;
                    blue = parseInt(hex.slice(6, 8), 16) / 255;
                }

                colors.push(new vscode.ColorInformation(range, new vscode.Color(red, green, blue, alpha)));
            }
            return colors;
        },
        provideColorPresentations(color, context) {
            const r = Math.round(color.red * 255).toString(16).padStart(2, '0').toUpperCase();
            const g = Math.round(color.green * 255).toString(16).padStart(2, '0').toUpperCase();
            const b = Math.round(color.blue * 255).toString(16).padStart(2, '0').toUpperCase();
            const a = Math.round(color.alpha * 255).toString(16).padStart(2, '0').toUpperCase();

            let label = `0x${r}${g}${b}`;
            // Preserve 8-digit format if it was written or if alpha has transparency
            if (color.alpha < 1.0 || (context.range.end.character - context.range.start.character) === 10) {
                label = `0x${a}${r}${g}${b}`;
            }
            return [new vscode.ColorPresentation(label)];
        }
    };
}

export function activate(context: vscode.ExtensionContext): void {
    // Highlighting comes from the grammar and the colour picker from here, so
    // both work whether or not the server is found.
    context.subscriptions.push(vscode.languages.registerColorProvider('byld', colorProvider()));

    context.subscriptions.push(
        vscode.commands.registerCommand('byld.restartServer', () => restartServer(context))
    );

    context.subscriptions.push(
        vscode.workspace.onDidChangeConfiguration((e) => {
            if (e.affectsConfiguration('byld.lsp.serverPath')) {
                void restartServer(context);
            }
        })
    );

    void startServer(context, false);
}

export function deactivate(): Thenable<void> | undefined {
    return stopServer();
}
