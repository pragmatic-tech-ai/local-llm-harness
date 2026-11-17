import { Observable } from '@pragmatic-tech-ai/mural/runtime'

/** Placeholder content shown in the ContentHost until the chat UI arrives. */
export class PlaceholderViewModel extends Observable
{
    private static readonly MessageText = 'local-llm-harness — shell up. Chat UI arrives in Task 7b.'

    public get Message(): string
    {
        return PlaceholderViewModel.MessageText
    }
}
