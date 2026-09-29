// AudioWorklet processor for the live voice report (voice_controller.js).
//
// Runs at the AudioContext's native rate (usually 44.1 or 48 kHz). Downmixes the microphone to
// mono, resamples it to 16 kHz, converts it to 16-bit little-endian PCM and posts one
// ArrayBuffer (transferred) per ~100 ms to the main thread, which base64-encodes it for
// Gemini Live (`audio/pcm;rate=16000`).
//
// Resampling is by area averaging: each output sample is the mean of the input it covers, with
// fractional weights at the edges. The box filter this implies is a crude but cheap
// anti-aliasing low-pass — plenty for speech recognition — and it also works (as a sample-and-
// hold) should the context run below 16 kHz.

class Resampler {
  constructor(inputRate, outputRate) {
    this.ratio = inputRate / outputRate // input samples per output sample
    this.filled = 0                     // input samples accumulated in the current output bin
    this.sum = 0
  }

  // Calls emit(sample) for each completed output sample.
  push(samples, emit) {
    const ratio = this.ratio

    for (let i = 0; i < samples.length; i++) {
      const x = samples[i]
      let left = 1

      while (left > 0) {
        const room = ratio - this.filled

        if (left < room) {
          this.sum += x * left
          this.filled += left
          left = 0
        } else {
          this.sum += x * room
          emit(this.sum / ratio)
          this.sum = 0
          this.filled = 0
          left -= room
        }
      }
    }
  }
}

const Base = globalThis.AudioWorkletProcessor || class {}

class PcmCaptureProcessor extends Base {
  constructor(options = {}) {
    super(options)

    const { targetRate = 16000, chunkMs = 100 } = options.processorOptions || {}
    this.resampler = new Resampler(sampleRate, targetRate)
    this.chunkSamples = Math.max(1, Math.round(targetRate * chunkMs / 1000))
    this.#newChunk()
    this.emit = (sample) => this.#write(sample)
    this.mono = new Float32Array(128)
    this.stopped = false

    this.port.onmessage = ({ data }) => {
      if (data === "stop") this.stopped = true
    }
  }

  process(inputs) {
    if (this.stopped) return false

    const channels = inputs[0]
    if (channels && channels.length > 0 && channels[0].length > 0) {
      this.resampler.push(this.#downmix(channels), this.emit)
    }

    return true
  }

  #downmix(channels) {
    if (channels.length === 1) return channels[0]

    const length = channels[0].length
    if (this.mono.length !== length) this.mono = new Float32Array(length)

    const mono = this.mono
    mono.fill(0)
    for (const channel of channels) {
      for (let i = 0; i < length; i++) mono[i] += channel[i]
    }
    for (let i = 0; i < length; i++) mono[i] /= channels.length

    return mono
  }

  #write(sample) {
    const clamped = Math.max(-1, Math.min(1, sample))
    const int = clamped < 0 ? Math.round(clamped * 0x8000) : Math.round(clamped * 0x7fff)
    this.view.setInt16(this.offset, int, true)
    this.offset += 2

    if (this.offset === this.buffer.byteLength) {
      this.port.postMessage(this.buffer, [ this.buffer ])
      this.#newChunk()
    }
  }

  #newChunk() {
    this.buffer = new ArrayBuffer(this.chunkSamples * 2)
    this.view = new DataView(this.buffer)
    this.offset = 0
  }
}

if (typeof registerProcessor === "function") {
  registerProcessor("pcm-capture", PcmCaptureProcessor)
}
