export {}

declare global
{
    interface Window
    {
        harness: { BackendUrl(): Promise<string | null>, SupervisorUrl(): Promise<string | null> }
    }
}
