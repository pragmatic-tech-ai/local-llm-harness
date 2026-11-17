import { afterEach, describe, it, expect } from 'vitest'
import * as fs from 'node:fs'
import * as os from 'node:os'
import * as path from 'node:path'
import { Sidecar } from './sidecar'

class SidecarTestFixture
{
    public static readonly StubSource =
        "console.log('listening on ws://127.0.0.1:9999 (model m, n_ctx 1, upstream u)');\n" +
        "setInterval(() => {}, 1000);\n"
    public static readonly ExpectedUrl = 'ws://127.0.0.1:9999'
    public static readonly DirPrefix = 'sidecar-test-'
    public static readonly StubName = 'stub.js'
    public static readonly ExitedMessage = 'exited before becoming ready (code 3)'

    private static readonly dirs: string[] = []

    public static WriteStub(): string
    {
        const dir = fs.mkdtempSync(path.join(os.tmpdir(), SidecarTestFixture.DirPrefix))
        SidecarTestFixture.dirs.push(dir)
        const file = path.join(dir, SidecarTestFixture.StubName)
        fs.writeFileSync(file, SidecarTestFixture.StubSource)
        return file
    }

    public static Cleanup(): void
    {
        for (const dir of SidecarTestFixture.dirs.splice(0))
        {
            fs.rmSync(dir, { recursive: true, force: true })
        }
    }
}

describe('Sidecar', () =>
{
    afterEach(() => SidecarTestFixture.Cleanup())

    it('SpawnsAndReportsReadyUrl', async () =>
    {
        const stub = SidecarTestFixture.WriteStub()
        const sidecar = new Sidecar({ binaryPath: process.execPath, prefixArgs: [stub] })
        const exited = new Promise<boolean>(resolve => sidecar.OnExit.Subscribe((_code, expected) => resolve(expected)))

        const result = await sidecar.Start()
        expect(result).toEqual({ url: SidecarTestFixture.ExpectedUrl })
        expect(sidecar.IsRunning).toBe(true)

        await sidecar.Stop()
        expect(sidecar.IsRunning).toBe(false)
        expect(await exited).toBe(true)
    })

    it('RejectsWhenProcessExitsBeforeReady', async () =>
    {
        const sidecar = new Sidecar({ binaryPath: process.execPath, prefixArgs: ['-e', 'process.exit(3)', '--'] })
        await expect(sidecar.Start()).rejects.toThrow(SidecarTestFixture.ExitedMessage)
    })
})
