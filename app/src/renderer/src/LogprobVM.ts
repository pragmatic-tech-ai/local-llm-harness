import { Observable } from '@pragmatic-tech-ai/mural/runtime'
import type { TokenInfo } from './HarnessClient'

/** One generated token with its log-probability, for the logprobs list. */
export class LogprobVM extends Observable
{
    private static readonly NotAvailable = 'n/a'
    // The backend uses f32::MIN (about -3.4e38) when no logprob was computed.
    private static readonly SentinelThreshold = -1e38
    private static readonly Decimals = 3

    constructor(private readonly info: TokenInfo)
    {
        super()
    }

    public get Token(): string
    {
        return this.info.token
    }

    public get LogprobDisplay(): string
    {
        const value = this.info.logprob
        if (!Number.isFinite(value) || value <= LogprobVM.SentinelThreshold)
        {
            return LogprobVM.NotAvailable
        }
        return value.toFixed(LogprobVM.Decimals)
    }
}
