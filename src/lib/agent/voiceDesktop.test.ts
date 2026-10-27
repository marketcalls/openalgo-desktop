/**
 * The desktop's voice wording and the check for a web view that cannot do
 * voice (decision 13: the Linux window without WebRTC says why).
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { desktopMicrophoneRefusal, platformOf, voiceUnavailableReason } from './voiceDesktop'

const shell = vi.hoisted(() => ({ isDesktopShell: vi.fn(() => true) }))
vi.mock('@/lib/desktop', () => shell)

const LINUX = 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko)'
const MAC = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15'
const WINDOWS = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Edg/130.0'

function onPlatform(userAgent: string) {
  vi.spyOn(navigator, 'userAgent', 'get').mockReturnValue(userAgent)
}

function webView({ webRtc, microphone }: { webRtc: boolean; microphone: boolean }) {
  vi.stubGlobal('RTCPeerConnection', webRtc ? class {} : undefined)
  Object.defineProperty(navigator, 'mediaDevices', {
    configurable: true,
    value: microphone ? { getUserMedia: async () => ({}) } : undefined,
  })
}

describe('voice in the app window', () => {
  beforeEach(() => {
    shell.isDesktopShell.mockReturnValue(true)
    vi.stubGlobal('isSecureContext', true)
  })

  afterEach(() => {
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
  })

  it('tells the operating systems apart', () => {
    expect(platformOf(LINUX)).toBe('linux')
    expect(platformOf(MAC)).toBe('mac')
    expect(platformOf(WINDOWS)).toBe('windows')
    expect(platformOf('Mozilla/5.0')).toBe('other')
  })

  it('says why a Linux window without WebRTC has no microphone, and where it works', () => {
    onPlatform(LINUX)
    webView({ webRtc: false, microphone: true })
    const reason = voiceUnavailableReason()
    expect(reason).toContain('Voice needs WebRTC')
    expect(reason).toContain('Linux')
    expect(reason).toMatch(/http:\/\/127\.0\.0\.1:\d+\/agent/)
    webView({ webRtc: true, microphone: false })
    expect(voiceUnavailableReason()).toContain('Voice needs WebRTC')
  })

  it('says nothing where the web view can do voice', () => {
    for (const ua of [LINUX, MAC, WINDOWS]) {
      onPlatform(ua)
      webView({ webRtc: true, microphone: true })
      expect(voiceUnavailableReason()).toBeNull()
    }
  })

  it('leaves a browser to find out on the press, as the web does', () => {
    shell.isDesktopShell.mockReturnValue(false)
    onPlatform(LINUX)
    webView({ webRtc: false, microphone: false })
    expect(voiceUnavailableReason()).toBeNull()
  })

  it('names the operating system switch for a blocked microphone', () => {
    onPlatform(MAC)
    expect(desktopMicrophoneRefusal('NotAllowedError', true)).toContain(
      'System Settings, Privacy and Security, Microphone'
    )
    onPlatform(WINDOWS)
    expect(desktopMicrophoneRefusal('NotAllowedError', true)).toContain(
      'Let desktop apps access your microphone'
    )
    expect(desktopMicrophoneRefusal('NotFoundError', true)).toBeNull()
    shell.isDesktopShell.mockReturnValue(false)
    expect(desktopMicrophoneRefusal('NotAllowedError', true)).toBeNull()
  })

  it('sends a browser on a plain http address to this computer at the app port', () => {
    shell.isDesktopShell.mockReturnValue(false)
    const sentence = desktopMicrophoneRefusal('', false) ?? ''
    expect(sentence).toContain(
      `on the computer it runs on, at http://127.0.0.1:${window.location.port || '5000'}.`
    )
  })
})
