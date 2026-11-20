import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { SettingsFile } from './settings'

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
        writeFileSync(join(dir, 'settings.json'), '{"a":1}')
        expect(SettingsFile.Resolve(dir)).toEqual({ a: 1 })
    })

    it('ResolveMissingFileReturnsEmpty', () =>
    {
        expect(SettingsFile.Resolve(dir)).toEqual({})
    })

    it('ResolveCorruptFileReturnsEmpty', () =>
    {
        writeFileSync(join(dir, 'settings.json'), '{not json')
        expect(SettingsFile.Resolve(dir)).toEqual({})
    })

    it('WriteThenResolveRoundTrips', async () =>
    {
        const values = { url: 'ws://x:1', n: 2 }
        await SettingsFile.Write(dir, values)
        expect(SettingsFile.Resolve(dir)).toEqual(values)
    })
})
