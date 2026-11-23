import { TextFormat } from './TextFormat'

/**
 * One entry of a choice list (a ComboBox or ListBox item): the value it stands for and the label shown. A ComboBox
 * shows `Label`. A value that is not among the offered ones (a template's model missing from the catalog, or one the
 * chosen runtime cannot load) is kept as a marked choice, so a form never silently changes it.
 */
export class ChoiceItem
{
    private static readonly MissingFormat = '{0} (not in catalog)'
    private static readonly NotServedFormat = '{0} (not served by {1})'

    public readonly Value: string
    public readonly Label: string

    constructor(value: string, label: string = value)
    {
        this.Value = value
        this.Label = label
    }

    /** A value the catalog does not offer, labelled as such. */
    public static Missing(value: string): ChoiceItem
    {
        return new ChoiceItem(value, TextFormat.Format(ChoiceItem.MissingFormat, value))
    }

    /** A catalog model the chosen runtime cannot load, labelled as such. */
    public static NotServed(value: string, runtime: string): ChoiceItem
    {
        return new ChoiceItem(value, TextFormat.Format(ChoiceItem.NotServedFormat, value, runtime))
    }
}
