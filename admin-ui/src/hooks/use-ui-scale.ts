import { useEffect, useState, useCallback } from 'react'
import { storage, type UiScale } from '@/lib/storage'

const SCALES: UiScale[] = [90, 100, 115, 130]

function pickDefaultScale(): UiScale {
  const stored = storage.getUiScale()
  if (stored) return stored
  if (typeof window !== 'undefined' && window.innerWidth >= 2400) return 115
  return 100
}

function applyZoom(scale: UiScale) {
  const root = document.documentElement as HTMLElement & { style: CSSStyleDeclaration & { zoom?: string } }
  root.style.zoom = String(scale / 100)
}

export function useUiScale() {
  const [scale, setScaleState] = useState<UiScale>(() => pickDefaultScale())

  useEffect(() => {
    applyZoom(scale)
  }, [scale])

  const setScale = useCallback((next: UiScale) => {
    setScaleState(next)
    storage.setUiScale(next)
  }, [])

  return { scale, setScale, scales: SCALES }
}
