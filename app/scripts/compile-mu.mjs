import { execFileSync } from 'node:child_process'
import { globSync, existsSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const CLI_REL = '@pragmatic-tech-ai/mural/dist/tooling/cli.js'
const candidates = [
    new URL(`../node_modules/${CLI_REL}`, import.meta.url),
    new URL(`../../../node_modules/${CLI_REL}`, import.meta.url),
].map(fileURLToPath)
const cli = candidates.find((p) => existsSync(p))
if (cli === undefined) { console.error('[compile-mu] cannot find mural CLI in:', candidates); process.exit(1) }
const files = globSync('src/**/*.mu')
if (files.length === 0) { console.warn('[compile-mu] no .mu files found under src/'); process.exit(0) }
execFileSync(process.execPath, [cli, 'compile', ...files], { stdio: 'inherit' })
