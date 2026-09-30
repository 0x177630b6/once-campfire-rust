// Hermes fork: the Duty Manager Workspace's page behaviour (docs/hermes-workspace.md).
//
// Loaded as a module by the tab bar partial, which the layout renders only while the workspace is
// on, so it runs once per document; everything works by delegation and by watching the document,
// so it keeps working across Turbo navigations and for messages that arrive over Action Cable.
//
// - Card chips: links marked `data-ws-card` (chips, and card links the server didn't know yet) are
//   refreshed from GET /workspace/cards.json, so a chip shows the card's current state even inside
//   a cached message fragment.
// - Draft buttons (`.ws-draft`): File / Dismiss POST the decision; the server posts "confirm" or
//   "cancel" in the room as the user, mentioning the bot. Edit puts the mention and "change: " in the
//   composer. A draft followed by a later message from the same bot is marked answered.

const CARDS_URL = "/workspace/cards.json"
const REFRESH_MS = 60_000
const MAX_CHIPS = 50

let scanTimer = null

function scheduleScan(delay = 250) {
  clearTimeout(scanTimer)
  scanTimer = setTimeout(scan, delay)
}

function scan() {
  markAnsweredDrafts()
  refreshChips()
}

// --- Card chips ------------------------------------------------------------------------------------

async function refreshChips() {
  const links = [ ...document.querySelectorAll("a[data-ws-card]") ]
  const numbers = [ ...new Set(links.map(link => link.dataset.wsCard)) ].filter(n => /^\d+$/.test(n)).slice(0, MAX_CHIPS)
  if (numbers.length === 0) return

  const url = document.querySelector(".ws-tabbar")?.dataset.wsCardsUrl || CARDS_URL
  let cards
  try {
    const response = await fetch(`${url}?numbers=${numbers.join(",")}`, { credentials: "same-origin", headers: { "Accept": "application/json" } })
    if (!response.ok) return
    cards = (await response.json())?.cards || {}
  } catch {
    return // Fizzy or the network is down: links stay as they are
  }

  for (const link of document.querySelectorAll("a[data-ws-card]")) {
    const html = cards[link.dataset.wsCard]
    if (!html) continue
    const version = hash(html)
    if (link.dataset.wsV === version) continue
    const template = document.createElement("template")
    template.innerHTML = html
    const chip = template.content.firstElementChild
    if (!chip) continue
    chip.dataset.wsV = version
    link.replaceWith(chip)
  }
}

// Tells a chip we put in from one the server rendered, so that swapping it in doesn't trigger
// another refresh.
function hash(text) {
  let h = 5381
  for (let i = 0; i < text.length; i++) h = ((h * 33) ^ text.charCodeAt(i)) >>> 0
  return h.toString(36)
}

// --- Drafts -----------------------------------------------------------------------------------------

function markAnsweredDrafts() {
  for (const draft of document.querySelectorAll(".ws-draft:not([data-ws-draft-state])")) {
    const message = draft.closest(".message")
    if (!message) continue
    for (let next = message.nextElementSibling; next; next = next.nextElementSibling) {
      if (next.classList.contains("message") && next.dataset.userId === message.dataset.userId) {
        draft.dataset.wsDraftState = "superseded"
        break
      }
    }
  }
}

function setStatus(draft, text) {
  const status = draft.querySelector(".ws-draft__status")
  if (status) status.textContent = text
}

async function answerDraft(draft, decision) {
  draft.dataset.wsDraftState = "busy"
  setStatus(draft, decision === "confirm" ? "Filing…" : "Dismissing…")
  try {
    const response = await fetch(draft.dataset.wsDraftUrl, {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", "Accept": "application/json" },
      body: JSON.stringify({ decision })
    })
    const body = await response.json().catch(() => ({}))
    if (!response.ok) throw new Error(body?.message || `HTTP ${response.status}`)
    draft.dataset.wsDraftState = "sent"
    setStatus(draft, decision === "confirm" ? "Sent “confirm” to Hermes." : "Sent “cancel” to Hermes.")
  } catch (error) {
    delete draft.dataset.wsDraftState
    setStatus(draft, `Couldn’t send: ${error.message}`)
  }
}

function escapeHTML(text) {
  const element = document.createElement("span")
  element.textContent = text
  return element.innerHTML
}

function escapeAttribute(text) {
  return escapeHTML(text).replaceAll("\"", "&quot;")
}

// The composer's content: a mention of the bot (the attachment the mention prompt inserts, so the
// bot gets the message in a shared room), then "change: ".
function editDraft(draft) {
  const form = document.querySelector("form#composer")
  const editor = form?.querySelector("[data-composer-target='text']")
  if (!editor) return

  const name = draft.dataset.wsBotName || "Hermes"
  const mention = `<span class="mention">${escapeHTML(name)}</span>`
  const content = `<p><action-text-attachment sgid="${escapeAttribute(draft.dataset.wsBotSgid || "")}" content-type="application/vnd.campfire.mention" content="${escapeAttribute(mention)}"></action-text-attachment> change: </p>`

  const composer = window.Stimulus?.getControllerForElementAndIdentifier(form, "composer")
  if (composer?.replaceMessageContent) {
    composer.replaceMessageContent(content)
  } else {
    editor.value = content
    editor.focus()
  }
}

document.addEventListener("click", event => {
  const button = event.target.closest?.("[data-ws-draft-action]")
  const draft = button?.closest(".ws-draft")
  if (!draft || draft.dataset.wsDraftState === "busy") return

  event.preventDefault()
  const action = button.dataset.wsDraftAction
  if (action === "edit") {
    editDraft(draft)
  } else {
    answerDraft(draft, action)
  }
})

// --- Wiring ----------------------------------------------------------------------------------------

// The whole document, not the body: Turbo swaps the body on every visit.
new MutationObserver(mutations => {
  const relevant = mutations.some(mutation => [ ...mutation.addedNodes ].some(node =>
    node.nodeType === Node.ELEMENT_NODE && !node.dataset.wsV &&
      (node.matches("a[data-ws-card], .ws-draft, .message") || node.querySelector("a[data-ws-card], .ws-draft"))
  ))
  if (relevant) scheduleScan()
}).observe(document.documentElement, { childList: true, subtree: true })

document.addEventListener("turbo:load", () => scheduleScan(0))
document.addEventListener("visibilitychange", () => { if (document.visibilityState === "visible") scheduleScan(0) })
setInterval(() => { if (document.visibilityState === "visible") refreshChips() }, REFRESH_MS)

scheduleScan(0)
