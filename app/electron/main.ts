import { app } from 'electron';
import { Sidecar } from './sidecar';

/** Holds the resolved backend url for the renderer (UI arrives in Task 7). */
export class SidecarHolder
{
    private static readonly ReadyEvent = 'ready';
    private static readonly BeforeQuitEvent = 'before-quit';
    private static readonly ExitMessage = 'sidecar exited unexpectedly: ';
    private static readonly StartFailedMessage = 'sidecar failed to start: ';

    public Url: string | null = null;
    private readonly sidecar = new Sidecar();
    private quitting = false;

    public Attach(): void
    {
        app.on(SidecarHolder.ReadyEvent, () => void this.OnReady());
        app.on(SidecarHolder.BeforeQuitEvent, event => void this.OnBeforeQuit(event));
        this.sidecar.OnExit.Subscribe((code, expected) =>
        {
            if (!expected)
            {
                console.error(SidecarHolder.ExitMessage + code);
            }
        });
    }

    private async OnReady(): Promise<void>
    {
        try
        {
            const ready = await this.sidecar.Start();
            this.Url = ready.url;
        }
        catch (err)
        {
            console.error(SidecarHolder.StartFailedMessage + (err instanceof Error ? err.message : String(err)));
        }
    }

    private async OnBeforeQuit(event: { preventDefault(): void }): Promise<void>
    {
        if (this.quitting)
        {
            return;
        }
        this.quitting = true;
        event.preventDefault();
        await this.sidecar.Stop();
        app.quit();
    }
}

new SidecarHolder().Attach();
