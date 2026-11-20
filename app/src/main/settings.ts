import { readFileSync } from 'node:fs'
import { mkdir, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import type { IpcMain } from 'electron'
import { SettingsChannel } from '../shared/settings-api'

/** Persists the app settings to userData/settings.json; Resolve/Write are Electron-free over a passed-in directory. */
export class SettingsFile
{
    public static readonly FileName = 'settings.json'
    private static readonly Encoding = 'utf8'
    private static readonly JsonIndent = 2

    /** The parsed settings in dir, or {} when the file is missing or unparseable. */
    public static Resolve(dir: string): Record<string, unknown>
    {
        try
        {
            const parsed: unknown = JSON.parse(readFileSync(join(dir, SettingsFile.FileName), SettingsFile.Encoding))
            if (parsed && typeof parsed === 'object' && !Array.isArray(parsed))
            {
                return parsed as Record<string, unknown>
            }
            return {}
        }
        catch
        {
            return {}
        }
    }

    /** Writes values to dir/settings.json as pretty-printed JSON, creating dir if needed. */
    public static async Write(dir: string, values: Record<string, unknown>): Promise<void>
    {
        await mkdir(dir, { recursive: true })
        await writeFile(join(dir, SettingsFile.FileName), JSON.stringify(values, null, SettingsFile.JsonIndent), SettingsFile.Encoding)
    }

    /** Wires the snapshot (sync) and save (async) channels. */
    public static Register(ipcMain: Pick<IpcMain, 'on' | 'handle'>, userDataDir: string): void
    {
        ipcMain.on(SettingsChannel.GetSnapshot, event =>
        {
            event.returnValue = SettingsFile.Resolve(userDataDir)
        })
        ipcMain.handle(SettingsChannel.Save, async (_event, values: Record<string, unknown>) =>
        {
            await SettingsFile.Write(userDataDir, values)
        })
    }
}
