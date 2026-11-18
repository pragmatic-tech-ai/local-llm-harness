import { describe, expect, it } from 'vitest'
import { SupervisorUrl } from './supervisor-url'

describe('SupervisorUrl', () =>
{
    it('ResolveSupervisorUrlPrefersEnv', () =>
    {
        expect(SupervisorUrl.Resolve({ HARNESS_SUPERVISOR_URL: 'ws://192.168.1.101:8780' })).toBe('ws://192.168.1.101:8780')
        expect(SupervisorUrl.Resolve({})).toBeNull()
        expect(SupervisorUrl.Resolve({ HARNESS_SUPERVISOR_URL: '' })).toBeNull()
        expect(SupervisorUrl.Resolve({ HARNESS_SUPERVISOR_URL: '   ' })).toBeNull()
        expect(SupervisorUrl.Resolve({ HARNESS_SUPERVISOR_URL: '  ws://192.168.1.101:8780  ' })).toBe('ws://192.168.1.101:8780')
    })
})
