import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vitest/config'

/**
 * Unit tests only. mural's dist breaks when Vite TRANSFORMS it (circular init:
 * "Class extends undefined"), and its package exports nest an import.development
 * condition pointing at uncompiled src. Alias every mural subpath to its built
 * dist (an absolute node_modules path is externalized, so Node loads it natively).
 */
class VitestSetup
{
    private static readonly Conditions = ['import', 'module', 'browser', 'default']
    private static readonly MuralDist = fileURLToPath(new URL('./node_modules/@pragmatic-tech-ai/mural/dist', import.meta.url))

    public static Config()
    {
        return defineConfig({
            resolve: {
                conditions: VitestSetup.Conditions,
                alias: [
                    { find: /^@pragmatic-tech-ai\/mural$/, replacement: `${VitestSetup.MuralDist}/index.js` },
                    { find: /^@pragmatic-tech-ai\/mural\/(.*)$/, replacement: `${VitestSetup.MuralDist}/$1` },
                ],
            },
            ssr: { resolve: { conditions: VitestSetup.Conditions } },
            test: { include: ['src/**/*.test.ts'], environment: 'node' },
        })
    }
}

export default VitestSetup.Config()
