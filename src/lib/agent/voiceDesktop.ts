/**
 * Voice in OpenAlgo Desktop: what the web's voice files say and check
 * differently because the page runs in the app's own window.
 *
 * The web's sentences send a trader to a browser's site settings and to
 * `http://127.0.0.1:5000`. In the app the microphone is the operating
 * system's to allow, for OpenAlgo Desktop itself, and the app may be on
 * another port. And the window's web view decides whether voice can work at
 * all: WebView2 (Windows) and WKWebView (macOS) provide WebRTC, while the
 * WebKitGTK web view on Linux does only where it was built with it.
 *
 * The same page also opens in an ordinary browser pointed at the app, where
 * the web's own sentences and behaviour apply unchanged; every function here
 * answers null there.
 */

import { isDesktopShell } from '@/lib/desktop'

type Platform = 'mac' | 'windows' | 'linux' | 'other'

/** The operating system the page runs on, from the user agent. */
export function platformOf(userAgent: string = navigator.userAgent): Platform {
  if (/Mac OS X|Macintosh/.test(userAgent)) return 'mac'
  if (/Windows/.test(userAgent)) return 'windows'
  if (/Linux|X11/.test(userAgent)) return 'linux'
  return 'other'
}

/** This app's address on this computer, at the port the page came from. */
function localAddress(): string {
  return `http://127.0.0.1:${window.location.port || '5000'}`
}

/**
 * The desktop's sentence for a microphone that could not be opened, or null
 * when the web's own sentence is the right one.
 *
 * @param name - The `getUserMedia` rejection's name, or empty.
 * @param secure - Whether the page is a secure context.
 */
export function desktopMicrophoneRefusal(name: string, secure: boolean): string | null {
  if (!secure || name === 'SecurityError') {
    // A browser on another device, over plain http: the web's sentence with
    // the app's own port.
    return (
      'Your browser will not allow the microphone on this address. Open ' +
      `OpenAlgo on the computer it runs on, at ${localAddress()}. A plain http ` +
      'address that is not on this machine can never use a microphone, whatever ' +
      'the settings say.'
    )
  }
  if (name !== 'NotAllowedError' || !isDesktopShell()) return null
  switch (platformOf()) {
    case 'mac':
      return (
        'The microphone was blocked. Allow OpenAlgo Desktop in System Settings, ' +
        'Privacy and Security, Microphone, then quit and reopen OpenAlgo.'
      )
    case 'windows':
      return (
        'The microphone was blocked. In Windows Settings, Privacy and security, ' +
        'Microphone, turn on microphone access and "Let desktop apps access your ' +
        'microphone", then press the microphone again.'
      )
    default:
      return (
        'The microphone was blocked. Check that this computer has a working ' +
        'microphone and that nothing in its privacy or sound settings blocks it, ' +
        'then press the microphone again.'
      )
  }
}

/**
 * Why voice cannot work in this window at all, or null when it can (or when
 * the page is in a browser, which finds out on the first press, as on the web).
 *
 * Desktop (decision 13, a deliberate deviation): where the web view offers no
 * WebRTC, the voice button says so instead of rendering nothing, so a Linux
 * trader who switched voice on learns why there is no microphone and where it
 * does work.
 */
export function voiceUnavailableReason(): string | null {
  if (!isDesktopShell() || !window.isSecureContext) return null
  const hasMicrophone = Boolean(navigator.mediaDevices?.getUserMedia)
  const hasWebRtc = typeof RTCPeerConnection !== 'undefined'
  if (hasMicrophone && hasWebRtc) return null
  const elsewhere = `${localAddress()}/agent`
  if (platformOf() === 'linux') {
    return (
      'Voice needs WebRTC, which the web view on this Linux system does not ' +
      'provide, so this window cannot use a microphone. Open OpenAlgo in Chrome ' +
      `or Firefox at ${elsewhere} to talk to the agent there.`
    )
  }
  return (
    "Voice cannot start in this window: this computer's web view does not offer " +
    'a microphone connection. Update the operating system, or open OpenAlgo in ' +
    `Chrome, Edge or Firefox at ${elsewhere} to talk to the agent there.`
  )
}
