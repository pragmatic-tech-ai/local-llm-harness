import { TextFormat } from './TextFormat'

/**
 * One entry of a choice list (a ComboBox or ListBox item): the value it stands for and the label shown. A ComboBox
 * shows `Label`. A value that is not among the offered ones (a template's model missing from the catalog) is kept as
 * a marked choice, so a form never silently changes it.
 */
export class ChoiceItem
{
    private static readonly MissingFormat = '{0} (not in catalog)'

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
}
