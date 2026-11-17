import { app } from 'electron';
import { Sidecar } from './sidecar';

/** Holds the resolved backend url for the renderer (UI arrives in Task 7). */
export class SidecarHolder
{
    private static readonly ReadyEvent = 'ready';
    private static readonly WillQuitEvent = 'will-quit';
    private static readonly ExitEvent = 'exit';
    private static readonly ExitMessage = 'sidecar exited unexpectedly: ';

    public Url: string | null = null;
    private readonly sidecar = new Sidecar();

    public Attach(): void
    {
        app.on(SidecarHolder.ReadyEvent, () => void this.OnReady());
        app.on(SidecarHolder.WillQuitEvent, () => void this.sidecar.Stop());
        this.sidecar.OnExit.on(SidecarHolder.ExitEvent, (code: number | null, expected: boolean) =>
        {
            if (!expected)
            {
                console.error(SidecarHolder.ExitMessage + code);
            }
        });
    }

    private async OnReady(): Promise<void>
    {
        const ready = await this.sidecar.Start();
        this.Url = ready.url;
    }
}

new SidecarHolder().Attach();
