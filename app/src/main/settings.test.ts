import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { SettingsFile } from './settings'
import { SettingsChannel } from '../shared/settings-api'

describe('SettingsFile', () =>
{
    let dir = ''

    beforeEach(() =>
    {
        dir = mkdtempSync(join(tmpdir(), 'settings-test-'))
    })

    afterEach(() =>
    {
        rmSync(dir, { recursive: true, force: true })
    })

    it('ResolveReadsParsedFile', () =>
    {
        writeFileSync(join(dir, SettingsFile.FileName), '{"a":1}')
        expect(SettingsFile.Resolve(dir)).toEqual({ a: 1 })
    })

    it('ResolveMissingFileReturnsEmpty', () =>
    {
        expect(SettingsFile.Resolve(dir)).toEqual({})
    })

    it('ResolveCorruptFileReturnsEmpty', () =>
    {
        writeFileSync(join(dir, SettingsFile.FileName), '{not json')
        expect(SettingsFile.Resolve(dir)).toEqual({})
    })

    it('WriteThenResolveRoundTrips', async () =>
    {
        const values = { url: 'ws://x:1', n: 2 }
        await SettingsFile.Write(dir, values)
        expect(SettingsFile.Resolve(dir)).toEqual(values)
    })

    it('RegisterWiresSnapshotAndSaveChannels', async () =>
    {
        const onHandlers = new Map<string, (event: { returnValue?: unknown }) => void>()
        const handleHandlers = new Map<string, (event: unknown, values: Record<string, unknown>) => Promise<void>>()
        const stub = {
            on: (channel: string, handler: (event: { returnValue?: unknown }) => void) => { onHandlers.set(channel, handler) },
            handle: (channel: string, handler: (event: unknown, values: Record<string, unknown>) => Promise<void>) => { handleHandlers.set(channel, handler) }
        }
        writeFileSync(join(dir, SettingsFile.FileName), '{"a":1}')
        SettingsFile.Register(stub as never, dir)

        const event: { returnValue?: unknown } = {}
        onHandlers.get(SettingsChannel.GetSnapshot)!(event)
        expect(event.returnValue).toEqual({ a: 1 })

        await handleHandlers.get(SettingsChannel.Save)!({}, { b: 2 })
        expect(SettingsFile.Resolve(dir)).toEqual({ b: 2 })
    })
})
