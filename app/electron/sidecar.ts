import { ChildProcess, spawn } from 'node:child_process';
import { EventEmitter } from 'node:events';
import * as path from 'node:path';

export interface ISidecarOptions
{
    binaryPath?: string;
    bind?: string;
    serverUrl?: string;
    model?: string;
    /** Arguments placed before the backend flags (used to run a script via an interpreter). */
    prefixArgs?: string[];
    startTimeoutMs?: number;
}

export interface ISidecarReady
{
    url: string;
}

/**
 * Spawns and manages the Rust backend process. Raises OnExit ('exit' event with
 * (code, expected)) whenever the child exits; expected is true after Stop().
 */
export class Sidecar
{
    private static readonly DefaultBind = '127.0.0.1:8777';
    private static readonly DefaultServerUrl = 'http://192.168.1.101:8080';
    private static readonly DefaultModel = 'gemma-4-12b-qat';
    private static readonly DefaultBinaryRelative = '../backend/target/debug/backend.exe';
    private static readonly DefaultStartTimeoutMs = 30000;
    private static readonly BindFlag = '--bind';
    private static readonly ServerUrlFlag = '--server-url';
    private static readonly ModelFlag = '--model';
    private static readonly ReadyPrefix = 'listening on ';
    private static readonly UrlScheme = 'ws://';
    private static readonly ExitEvent = 'exit';
    private static readonly DataEvent = 'data';
    private static readonly ErrorEvent = 'error';
    private static readonly Newline = '\n';
    private static readonly ExitedBeforeReady = 'Sidecar exited before becoming ready (code ';
    private static readonly ExitedBeforeReadyEnd = ')';
    private static readonly StartTimedOut = 'Sidecar did not become ready in time';
    private static readonly AlreadyStarted = 'Sidecar already started';
    private static readonly KillSignal = 'SIGTERM';
    private static readonly ForceKillSignal = 'SIGKILL';
    private static readonly StopGraceMs = 5000;

    public readonly OnExit: EventEmitter = new EventEmitter();

    private readonly options: ISidecarOptions;
    private child: ChildProcess | null = null;
    private stopping = false;
    private url: string | null = null;

    public constructor(options: ISidecarOptions = {})
    {
        this.options = options;
    }

    public get IsRunning(): boolean
    {
        return this.child !== null;
    }

    public get Url(): string | null
    {
        return this.url;
    }

    public Start(): Promise<ISidecarReady>
    {
        if (this.child !== null)
        {
            return Promise.reject(new Error(Sidecar.AlreadyStarted));
        }
        // __dirname is app/dist/electron (compiled) or app/electron (tests); resolve from the app root.
        const binary = this.options.binaryPath ?? path.resolve(Sidecar.AppRoot(), Sidecar.DefaultBinaryRelative);
        const args = [
            ...(this.options.prefixArgs ?? []),
            Sidecar.BindFlag, this.options.bind ?? Sidecar.DefaultBind,
            Sidecar.ServerUrlFlag, this.options.serverUrl ?? Sidecar.DefaultServerUrl,
            Sidecar.ModelFlag, this.options.model ?? Sidecar.DefaultModel,
        ];
        const timeoutMs = this.options.startTimeoutMs ?? Sidecar.DefaultStartTimeoutMs;
        this.stopping = false;

        return new Promise<ISidecarReady>((resolve, reject) =>
        {
            const child = spawn(binary, args, { stdio: ['ignore', 'pipe', 'pipe'] });
            this.child = child;
            let ready = false;
            let buffer = '';

            const timer = setTimeout(() =>
            {
                if (!ready)
                {
                    child.kill();
                    reject(new Error(Sidecar.StartTimedOut));
                }
            }, timeoutMs);

            const onData = (chunk: Buffer): void =>
            {
                if (ready)
                {
                    return;
                }
                buffer += chunk.toString();
                const lines = buffer.split(Sidecar.Newline);
                buffer = lines.pop() ?? '';
                for (const line of lines)
                {
                    const url = Sidecar.ParseReadyLine(line);
                    if (url !== null)
                    {
                        ready = true;
                        clearTimeout(timer);
                        this.url = url;
                        resolve({ url });
                        return;
                    }
                }
            };
            child.stdout?.on(Sidecar.DataEvent, onData);
            child.stderr?.on(Sidecar.DataEvent, onData);

            child.on(Sidecar.ErrorEvent, err =>
            {
                clearTimeout(timer);
                this.child = null;
                if (!ready)
                {
                    reject(err);
                }
            });

            child.on(Sidecar.ExitEvent, code =>
            {
                clearTimeout(timer);
                this.child = null;
                this.url = null;
                if (!ready)
                {
                    reject(new Error(Sidecar.ExitedBeforeReady + code + Sidecar.ExitedBeforeReadyEnd));
                }
                this.OnExit.emit(Sidecar.ExitEvent, code, this.stopping);
            });
        });
    }

    public Stop(): Promise<void>
    {
        const child = this.child;
        if (child === null)
        {
            return Promise.resolve();
        }
        this.stopping = true;
        return new Promise<void>(resolve =>
        {
            const force = setTimeout(() => child.kill(Sidecar.ForceKillSignal), Sidecar.StopGraceMs);
            child.once(Sidecar.ExitEvent, () =>
            {
                clearTimeout(force);
                resolve();
            });
            child.kill(Sidecar.KillSignal);
        });
    }

    private static AppRoot(): string
    {
        const parent = path.basename(path.dirname(__dirname));
        return parent === 'dist' ? path.resolve(__dirname, '..', '..') : path.resolve(__dirname, '..');
    }

    private static ParseReadyLine(line: string): string | null
    {
        const trimmed = line.trim();
        if (!trimmed.startsWith(Sidecar.ReadyPrefix))
        {
            return null;
        }
        const rest = trimmed.substring(Sidecar.ReadyPrefix.length);
        if (!rest.startsWith(Sidecar.UrlScheme))
        {
            return null;
        }
        const end = rest.search(/\s/);
        return end < 0 ? rest : rest.substring(0, end);
    }
}
