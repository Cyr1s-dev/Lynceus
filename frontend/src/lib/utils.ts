import { type ClassValue, clsx } from "clsx"
import { twMerge } from "tailwind-merge"

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

/**
 * Prefer the AI-summarized mission title, falling back to the raw goal so
 * legacy missions (created before titles existed) still render a label.
 */
export function missionDisplayTitle(mission: {
  title?: string | null
  user_goal: string
}): string {
  const title = mission.title?.trim()
  return title ? title : mission.user_goal
}
