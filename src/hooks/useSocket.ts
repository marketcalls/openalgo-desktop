/**
 * Socket hook stub for OpenAlgo Desktop
 *
 * In the desktop version, real-time updates come from Tauri events
 * instead of Socket.IO. This stub provides a compatible interface.
 */

import { useCallback, useEffect, useRef } from 'react'
import { toast } from 'sonner'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { useAuthStore } from '@/stores/authStore'

// Audio throttling configuration
const AUDIO_THROTTLE_MS = 1000

interface OrderEventData {
  symbol: string
  action: string
  orderid: string
  batch_order?: boolean
  is_last_order?: boolean
}

export function useSocket() {
  const { isAuthenticated } = useAuthStore()
  const audioRef = useRef<HTMLAudioElement | null>(null)
  const lastAudioTimeRef = useRef<number>(0)
  const audioEnabledRef = useRef<boolean>(false)

  const playAlertSound = useCallback(() => {
    const now = Date.now()
    const timeSinceLastAttempt = now - lastAudioTimeRef.current

    if (timeSinceLastAttempt < AUDIO_THROTTLE_MS && lastAudioTimeRef.current !== 0) {
      return
    }

    lastAudioTimeRef.current = now

    if (audioRef.current) {
      audioRef.current
        .play()
        .then(() => {
          audioEnabledRef.current = true
        })
        .catch(() => {})
    }
  }, [])

  const enableAudio = useCallback(() => {
    if (!audioEnabledRef.current && audioRef.current) {
      const audio = audioRef.current
      const originalVolume = audio.volume
      audio.volume = 0
      audio
        .play()
        .then(() => {
          audio.pause()
          audio.currentTime = 0
          audio.volume = originalVolume
          audioEnabledRef.current = true
        })
        .catch(() => {
          audio.volume = originalVolume
        })
    }
  }, [])

  useEffect(() => {
    // Only connect when authenticated
    if (!isAuthenticated) {
      return
    }

    // Create audio element
    audioRef.current = new Audio('/sounds/alert.mp3')
    audioRef.current.preload = 'auto'

    // Enable audio on user interaction
    const handleInteraction = () => {
      enableAudio()
    }

    ;['click', 'touchstart', 'keydown'].forEach((eventType) => {
      document.addEventListener(eventType, handleInteraction, { once: true, passive: true })
    })

    // Listen to Tauri events instead of Socket.IO
    const unlisteners: UnlistenFn[] = []

    // Order event from Tauri backend
    listen<OrderEventData>('order_event', (event) => {
      const data = event.payload
      const shouldPlayAudio = !data.batch_order || data.is_last_order
      if (shouldPlayAudio) {
        playAlertSound()
      }

      const message = `${data.action.toUpperCase()} Order Placed for Symbol: ${data.symbol}, Order ID: ${data.orderid}`
      if (data.action.toUpperCase() === 'BUY') {
        toast.success(message)
      } else {
        toast.error(message)
      }
    }).then((unlisten) => unlisteners.push(unlisten))

    // Generic notification
    listen<{ message: string; type?: string }>('notification', (event) => {
      playAlertSound()
      const { message, type } = event.payload
      if (type === 'success') toast.success(message)
      else if (type === 'error') toast.error(message)
      else toast.info(message)
    }).then((unlisten) => unlisteners.push(unlisten))

    return () => {
      unlisteners.forEach((unlisten) => unlisten())
      ;['click', 'touchstart', 'keydown'].forEach((eventType) => {
        document.removeEventListener(eventType, handleInteraction)
      })
    }
  }, [isAuthenticated, playAlertSound, enableAudio])

  return {
    socket: null, // No Socket.IO in desktop mode
    playAlertSound,
  }
}
