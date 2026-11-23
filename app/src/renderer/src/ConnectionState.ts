/** The runtime connection as the Sessions capability reports it. */
export enum ConnectionState
{
    /** Open: lists and placements are live. */
    Connected = 'connected',
    /** Discovering the runtime or waiting to retry: what is shown may be stale. */
    Reconnecting = 'reconnecting',
    /** No supervisor url: nothing to connect to. */
    Disconnected = 'disconnected',
}
