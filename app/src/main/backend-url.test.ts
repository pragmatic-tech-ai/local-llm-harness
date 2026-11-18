import { describe, expect, it } from 'vitest'
import { BackendUrl } from './backend-url'

describe('BackendUrl', () =>
{
    it('ResolveBackendUrlPrefersEnv', () =>
    {
        expect(BackendUrl.Resolve({ HARNESS_BACKEND_URL: 'ws://192.168.1.101:8777' })).toBe('ws://192.168.1.101:8777')
        expect(BackendUrl.Resolve({})).toBeNull()
        expect(BackendUrl.Resolve({ HARNESS_BACKEND_URL: '' })).toBeNull()
    })
})
