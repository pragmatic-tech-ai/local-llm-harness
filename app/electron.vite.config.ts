import { resolve, join } from 'node:path'
import { existsSync, readdirSync, readFileSync, realpathSync } from 'node:fs'
import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'
import { defineConfig, externalizeDepsPlugin } from 'electron-vite'
import type { Plugin } from 'vite'

// The Pragmatic theme requests its woff2s from `assets/fonts/` next to the renderer bundle.
// They ship inside the mural package (build/resources/pragmatic/fonts) but nothing copies
// them into the renderer output, so emit them as build assets under the exact requested
// name (no content hash). Renderer build only.
class ThemeFontCopy
{
    private static readonly PluginName = 'harness:theme-font-copy'
    private static readonly PackageName = '@pragmatic-tech-ai/mural'
    private static readonly ManifestFile = 'package.json'
    private static readonly FontsDir = join('build', 'resources', 'pragmatic', 'fonts')
    private static readonly FontExtension = '.woff2'
    private static readonly OutputDir = 'assets/fonts/'
    private static readonly RequiredFonts = [
        'InterTight-Regular',
        'InterTight-Medium',
        'InterTight-SemiBold',
        'JetBrainsMono-Regular',
        'JetBrainsMono-Medium',
        'SourceSerif4-Regular',
    ]

    // mural's `exports` map does not expose package.json, so ask Node's own module lookup for
    // the node_modules search paths and take the first that holds mural, following symlinks.
    private static PackageDir(): string
    {
        const lookup = createRequire(fileURLToPath(import.meta.url)).resolve.paths(ThemeFontCopy.PackageName) ?? []
        for (const base of lookup)
        {
            const candidate = join(base, ThemeFontCopy.PackageName)
            if (existsSync(join(candidate, ThemeFontCopy.ManifestFile)))
            {
                return realpathSync(candidate)
            }
        }
        throw new Error(`${ThemeFontCopy.PackageName} not found on the module search path`)
    }

    private static Fonts(): { dir: string, files: string[] }
    {
        const dir = join(ThemeFontCopy.PackageDir(), ThemeFontCopy.FontsDir)
        const files = existsSync(dir)
            ? readdirSync(dir).filter((f) => f.endsWith(ThemeFontCopy.FontExtension))
            : []
        const missing = ThemeFontCopy.RequiredFonts.filter((f) => !files.includes(f + ThemeFontCopy.FontExtension))
        if (files.length === 0 || missing.length > 0)
        {
            throw new Error(`Pragmatic theme fonts missing in ${dir}: ${files.length === 0 ? 'no woff2 files found' : missing.join(', ')}`)
        }
        return { dir, files }
    }

    public static Plugin(): Plugin
    {
        return {
            name: ThemeFontCopy.PluginName,
            apply: 'build',
            generateBundle()
            {
                const fonts = ThemeFontCopy.Fonts()
                for (const file of fonts.files)
                {
                    this.emitFile({
                        type: 'asset',
                        fileName: ThemeFontCopy.OutputDir + file,
                        source: readFileSync(join(fonts.dir, file)),
                    })
                }
            },
        }
    }
}

// Renderer wiring for a pure-mural app: mural's `exports` carries an `import` condition only
// and its Pragmatic theme/scheme modules mis-order under the pre-bundler, so every mural
// subpath is excluded from optimizeDeps and the package is deduped by name.
class MuralRendererConfig
{
    private static readonly Mural = '@pragmatic-tech-ai/mural'
    private static readonly Subpaths = ['', '/runtime', '/basic', '/framework', '/visual-engine', '/tooling', '/resources/pragmatic']
    private static readonly Esbuild = 'esbuild'
    private static readonly WsNative = ['bufferutil', 'utf-8-validate']
    private static readonly HighlightJs = 'highlight.js'
    private static readonly RendererHtml = 'src/renderer/index.html'

    public static MainExternal(): string[]
    {
        return [MuralRendererConfig.Esbuild, ...MuralRendererConfig.WsNative]
    }

    public static MuralPackage(): string
    {
        return MuralRendererConfig.Mural
    }

    public static Conditions(): string[]
    {
        return ['import', 'module', 'browser', 'default']
    }

    public static Dedupe(): string[]
    {
        return [MuralRendererConfig.Mural]
    }

    public static OptimizeDepsExclude(): string[]
    {
        return MuralRendererConfig.Subpaths.map((s) => MuralRendererConfig.Mural + s)
    }

    public static OptimizeDepsInclude(): string[]
    {
        return [MuralRendererConfig.HighlightJs]
    }

    public static Input(): string
    {
        return resolve(MuralRendererConfig.RendererHtml)
    }
}

export default defineConfig({
    main: {
        plugins: [externalizeDepsPlugin({ exclude: [MuralRendererConfig.MuralPackage()] })],
        build: { rollupOptions: { external: MuralRendererConfig.MainExternal() } },
    },
    preload: {
        plugins: [externalizeDepsPlugin()],
    },
    renderer: {
        plugins: [ThemeFontCopy.Plugin()],
        resolve: {
            conditions: MuralRendererConfig.Conditions(),
            dedupe: MuralRendererConfig.Dedupe(),
        },
        optimizeDeps: {
            exclude: MuralRendererConfig.OptimizeDepsExclude(),
            include: MuralRendererConfig.OptimizeDepsInclude(),
        },
        build: {
            rollupOptions: { input: { index: MuralRendererConfig.Input() } },
        },
    },
})
