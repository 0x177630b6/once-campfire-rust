// Hermes fork: tests of the shared Gemini Live session (overrides/lib/hermes/live_session.js), with
// a fake WebSocket and Node's built-in runner: `node --test crates/assets/tests/js/*.test.mjs`.
import { test } from "node:test"
import assert from "node:assert/strict"

import * as live from "../../overrides/lib/hermes/live_session.js"

class FakeSocket {
  static all = []

  constructor(url) {
    this.url = url
    this.readyState = 0
    this.sent = []
    FakeSocket.all.push(this)
    setTimeout(() => {
      this.readyState = 1
      this.onopen?.()
    }, 0)
  }

  send(text) {
    this.sent.push(JSON.parse(text))
  }

  close() {
    this.readyState = 3
  }

  // A server message, then a turn of the event loop for the session's in-order decoding.
  async receive(message) {
    this.onmessage?.({ data: JSON.stringify(message) })
    await tick()
  }
}

const tick = () => new Promise(resolve => setTimeout(resolve, 0))

// A session whose sockets answer setupComplete as soon as the setup arrives.
function session(handlers = {}) {
  const tokens = []
  const created = []
  const live_ = new live.LiveSession({
    fetchToken: async (options) => {
      tokens.push(options)
      return { token: `auth_tokens/${tokens.length}`, ws_url: "wss://example.test/ws" }
    },
    createSocket: url => {
      const socket = new FakeSocket(url)
      const send = socket.send.bind(socket)
      socket.send = text => {
        send(text)
        if (JSON.parse(text).setup) setTimeout(() => socket.onmessage({ data: JSON.stringify({ setupComplete: {} }) }), 0)
      }
      created.push(socket)
      return socket
    },
    handlers
  })
  return { live: live_, tokens, created }
}

test("messages for push-to-talk", () => {
  assert.deepEqual(live.activityStartMessage(), { realtimeInput: { activityStart: {} } })
  assert.deepEqual(live.activityEndMessage(), { realtimeInput: { activityEnd: {} } })
  assert.deepEqual(live.contextMessage("[note]"), {
    clientContent: { turns: [ { role: "user", parts: [ { text: "[note]" } ] } ], turnComplete: false }
  })
  assert.deepEqual(live.setupMessage(null), { setup: {} })
  assert.deepEqual(live.setupMessage("h1"), { setup: { sessionResumption: { handle: "h1" } } })
  assert.deepEqual(live.audioMessage("AAA="), { realtimeInput: { audio: { data: "AAA=", mimeType: "audio/pcm;rate=16000" } } })
})

test("a turn: the screen note, then activityStart; activityEnd on release", async () => {
  const { live: s, tokens, created } = session()
  assert.equal(s.beginTurn("note"), false, "not connected yet")
  await s.start()
  assert.deepEqual(tokens, [ { resume: false } ])
  const socket = created[0]
  assert.equal(socket.url, "wss://example.test/ws?access_token=auth_tokens%2F1")
  assert.deepEqual(socket.sent[0], { setup: {} })
  assert.equal(s.open, true)

  assert.equal(s.beginTurn("[Screen note]"), true)
  s.sendAudio("AAA=")
  assert.equal(s.endTurn(), true)
  assert.deepEqual(socket.sent.slice(1), [
    live.contextMessage("[Screen note]"), live.activityStartMessage(), live.audioMessage("AAA="), live.activityEndMessage()
  ])
  assert.equal(s.beginTurn(""), true, "no note: only activityStart")
  assert.deepEqual(socket.sent.at(-1), live.activityStartMessage())
  s.close()
  assert.equal(s.open, false)
  assert.equal(s.endTurn(), false)
})

test("server content reaches the handlers, usage included", async () => {
  const seen = []
  const { live: s, created } = session({
    onAudio: data => seen.push([ "audio", data ]),
    onTranscript: (role, text) => seen.push([ role, text ]),
    onTurnComplete: () => seen.push([ "turnComplete" ]),
    onUsage: usage => seen.push([ "usage", usage.totalTokenCount ])
  })
  await s.start()
  await created[0].receive({ serverContent: { inputTranscription: { text: "what's urgent" } } })
  await created[0].receive({ serverContent: { modelTurn: { parts: [ { inlineData: { mimeType: "audio/pcm;rate=24000", data: "AQI=" } } ] }, outputTranscription: { text: "Two things." } } })
  await created[0].receive({ serverContent: { turnComplete: true }, usageMetadata: { totalTokenCount: 812 } })
  assert.deepEqual(seen, [
    [ "user", "what's urgent" ], [ "audio", "AQI=" ], [ "model", "Two things." ], [ "turnComplete" ], [ "usage", 812 ]
  ])
})

test("a tool call is answered with the handler's result", async () => {
  const { live: s, created } = session({ onToolCall: async ({ name, args }) => ({ result: `${name}:${args.n}` }) })
  await s.start()
  await created[0].receive({ toolCall: { functionCalls: [ { id: "c1", name: "read_card", args: { n: 57 } } ] } })
  await tick()
  assert.deepEqual(created[0].sent.at(-1), { toolResponse: { functionResponses: [ { id: "c1", name: "read_card", response: { result: "read_card:57" } } ] } })
})

test("goAway reconnects once, with a resumption handle and one token", async () => {
  const events = []
  const { live: s, tokens, created } = session({ onReconnected: ({ resumed }) => events.push(resumed) })
  await s.start()
  await created[0].receive({ sessionResumptionUpdate: { newHandle: "h-1", resumable: true } })
  await created[0].receive({ goAway: { timeLeft: "5s" } })
  for (let i = 0; i < 5 && events.length === 0; i++) await tick()
  assert.deepEqual(tokens, [ { resume: false }, { resume: true } ], "exactly one more token")
  assert.equal(created.length, 2)
  assert.deepEqual(created[1].sent[0], { setup: { sessionResumption: { handle: "h-1" } } })
  assert.deepEqual(events, [ true ])
  assert.equal(created[0].readyState, 3, "the old socket is closed")
  s.close()
})

test("a token failure surfaces as a TokenError-shaped rejection", async () => {
  const s = new live.LiveSession({ fetchToken: async () => { throw new live.TokenError(429) }, createSocket: () => assert.fail("no socket") })
  await assert.rejects(s.start(), error => error instanceof live.TokenError && error.status === 429)
})
