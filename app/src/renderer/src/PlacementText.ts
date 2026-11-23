import { PlacementState, type Placement } from './agents'
import { TextFormat } from './TextFormat'

/** The one rendering of a placement as text, shared by the session list rows and the session header. */
export class PlacementText
{
    private static readonly UnavailableText = 'unavailable'
    private static readonly UnavailableFormat = 'unavailable: {0}'

    /** `unavailable: {reason}`, or `unavailable` without one. */
    public static Unavailable(placement: Placement): string
    {
        return placement.reason ? TextFormat.Format(PlacementText.UnavailableFormat, placement.reason) : PlacementText.UnavailableText
    }

    public static IsReady(placement: Placement | null): boolean
    {
        return placement?.state === PlacementState.Ready
    }

    public static NeedsLoad(placement: Placement | null): boolean
    {
        return placement?.state === PlacementState.NeedsLoad
    }
}
