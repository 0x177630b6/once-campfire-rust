// Hermes fork: the Duty Manager Workspace's page logic that doesn't touch the page
// (docs/hermes-workspace.md), apart from hermes/workspace.js so that Node's own test runner can test
// it without a browser or any npm package: `node --test crates/assets/tests/js/*.test.mjs`.
//
// The tab bar loads this module before hermes/workspace.js (module scripts run in document order),
// which reads it from `globalThis.HermesWorkspace`: asset URLs are digested, so workspace.js can't
// import it by a relative path.

export const SIGNED_OUT = "You’re signed out. Sign in again, then retry."
export const UNREACHABLE = "The server can’t be reached. Check your connection and retry."
export const MAX_CHIPS = 50

// Tells a chip we put in from one the server rendered, so that swapping it in doesn't trigger
// another refresh (djb2-xor, base 36).
export function hash(text) {
  let h = 5381
  for (let i = 0; i < text.length; i++) h = ((h * 33) ^ text.charCodeAt(i)) >>> 0
  return h.toString(36)
}

// The card numbers of the chip links on the page: digits only, once each, at most `max`.
export function chipNumbers(values, max = MAX_CHIPS) {
  return [ ...new Set(values) ].filter(n => /^\d+$/.test(String(n))).slice(0, max)
}

// Proposal ids of the drafts on the page, once each (what the server accepts: [a-z0-9]{1,40}).
export function proposalIds(values, max = 50) {
  return [ ...new Set(values) ].filter(id => /^[a-z0-9]{1,40}$/.test(String(id))).slice(0, max)
}

// What went wrong with a JSON reply, or null when it's fine. A redirect or a 401 means the session
// expired: the session check redirects to the sign-in page, which fetch follows (200, HTML).
export function replyProblem({ status, redirected, ok }, body = {}) {
  if (redirected || status === 401) return SIGNED_OUT
  if (ok === false || status >= 400) return body?.message || `Something went wrong (HTTP ${status}).`
  return null
}

// A draft's status line while its answer is on its way.
export function draftBusyText(decision) {
  return decision === "confirm" ? "Filing…" : "Dismissing…"
}

// A draft's status line once answered: a Hermes proposal says what the server did ("Card #14
// filed."); a text draft says what was posted in the room.
export function draftSentText(decision, body = {}, proposal = false) {
  if (proposal) return body?.message || (decision === "confirm" ? "Done." : "Dismissed.")
  return decision === "confirm" ? "Sent “confirm” to Hermes." : "Sent “cancel” to Hermes."
}

// A proposal's state from GET /workspace/hermes/proposals.json: null while it waits (the buttons
// stay), else the line to show instead of them.
export function proposalStateText(state) {
  if (!state || state.status === "pending") return null
  return state.label || state.status
}

// The body of a card sheet's change (POST /workspace/cards/:n/<kind>), from the control's values.
export function changeBody(kind, { value, checked, checkedValues = [], stepId } = {}) {
  switch (kind) {
    case "severity": return { severity: value }
    case "move": return { to: value }
    case "departments": return { tags: [ ...checkedValues ] }
    case "step": return { step_id: stepId, completed: Boolean(checked) }
  }
  return null
}

// The settings form as POST /workspace/settings reads it. `autonomy` maps action → dial; ids are
// numbers.
export function settingsBody({ departments = [], listed = false, managers = [], policy, autonomy = {}, hermesUserId = "" } = {}) {
  return {
    departments: departments.map(department => ({
      name: department.name,
      tag: department.tag,
      rooms: (department.rooms || []).map(Number).filter(Number.isFinite)
    })),
    duty_managers: listed ? managers.map(Number).filter(Number.isFinite) : null,
    confirm_policy: policy || "anyone",
    autonomy: { ...autonomy },
    hermes_fizzy_user_id: String(hermesUserId || "").trim() || null
  }
}

globalThis.HermesWorkspace = {
  SIGNED_OUT, UNREACHABLE, MAX_CHIPS, hash, chipNumbers, proposalIds, replyProblem, draftBusyText, draftSentText,
  proposalStateText, changeBody, settingsBody
}
