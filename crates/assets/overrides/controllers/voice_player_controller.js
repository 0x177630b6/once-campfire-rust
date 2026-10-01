import { Controller } from "@hotwired/stimulus"

// Hermes fork: an audio attachment's inline player (hermes::audio_preview). Writes the duration
// next to "Voice message" once the browser knows it: from the metadata (preload="metadata"),
// or, for recordings whose header has none (Chrome's MediaRecorder WebM), after a first play.

const pad = (n) => String(n).padStart(2, "0")

// 7.9 → "0:07" (like the player), 125 → "2:05"; null when unknown.
export function formatDuration(seconds) {
  if (!Number.isFinite(seconds) || seconds <= 0) return null
  const whole = Math.floor(seconds)
  return `${Math.floor(whole / 60)}:${pad(whole % 60)}`
}

export default class extends Controller {
  static targets = [ "audio", "duration" ]

  audioTargetConnected(audio) {
    this.update = () => this.#render(audio)
    for (const event of [ "loadedmetadata", "durationchange" ]) audio.addEventListener(event, this.update)
    this.#render(audio)
  }

  audioTargetDisconnected(audio) {
    for (const event of [ "loadedmetadata", "durationchange" ]) audio.removeEventListener(event, this.update)
  }

  #render(audio) {
    if (!this.hasDurationTarget) return
    const duration = formatDuration(audio.duration)
    this.durationTarget.textContent = duration ? ` · ${duration}` : ""
  }
}
