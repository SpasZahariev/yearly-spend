import { clsx, type ClassValue } from "clsx"
import { twMerge } from "tailwind-merge"

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

export function anchorFromClick(event: {
  clientX?: number
  clientY?: number
  currentTarget: Element
}) {
  const x = event.clientX ?? 0
  const y = event.clientY ?? 0
  if (x > 0 || y > 0) {
    return { x, y }
  }
  const rect = event.currentTarget.getBoundingClientRect()
  return {
    x: rect.left + rect.width / 2,
    y: rect.top + rect.height / 2,
  }
}
