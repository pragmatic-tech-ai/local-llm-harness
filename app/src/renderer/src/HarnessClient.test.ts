import { describe, expect, it } from 'vitest'
import { HarnessClient, type ModelProps, type TurnChunk } from './HarnessClient'

describe('HarnessClient', () =>
{
    it('MapsTurnChunkNotificationsToText', () =>
    {
        const client = new HarnessClient()
        const chunks: TurnChunk[] = []
        const order: string[] = []
        client.OnTurnChunk.Subscribe(p =>
        {
            chunks.push(p)
            order.push('chunk')
        })
        client.OnTurnDone.Subscribe(() => order.push('done'))

        client.HandleFrame(JSON.stringify({ method: 'turn_started', params: { prompt: 'ping', prompt_tokens: 1, n_ctx: 4096 } }))
        client.HandleFrame(JSON.stringify({ method: 'turn_chunk', params: { text: 'po', tokens: [], stop: false } }))
        client.HandleFrame(JSON.stringify({ method: 'turn_chunk', params: { text: 'ng', tokens: [], stop: true } }))
        client.HandleFrame(JSON.stringify({ method: 'turn_done', params: {} }))

        expect(chunks.map(c => c.text)).toEqual(['po', 'ng'])
        expect(order).toEqual(['chunk', 'chunk', 'done'])
    })

    it('ResolvesModelPropsById', async () =>
    {
        const client = new HarnessClient()
        const pending = client.ModelProps()
        const expected: ModelProps = { n_ctx: 4096, model_name: 'm' }
        client.HandleFrame(JSON.stringify({ id: 1, result: expected }))
        await expect(pending).resolves.toEqual(expected)
    })

    it('RejectsOnErrorResponse', async () =>
    {
        const client = new HarnessClient()
        const pending = client.Tokenize('x')
        client.HandleFrame(JSON.stringify({ id: 1, error: { code: -1, message: 'boom' } }))
        await expect(pending).rejects.toThrow('boom')
    })
})
