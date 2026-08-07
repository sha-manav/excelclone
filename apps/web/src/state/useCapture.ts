/**
 * Binds the capture pipeline to the React tree.
 *
 * The pipeline is a module-level singleton rather than component state, for a
 * reason StrictMode makes obvious: a controller and an offline queue are
 * per-tab resources, and a double-invoked render must not produce two of them
 * fighting over the same IndexedDB store. The hook is a thin, re-mountable
 * view onto that one instance.
 */

import { useCallback, useEffect, useState } from 'react'
import {
  CaptureController,
  CONSENT_TEXT_VERSION,
  ulid,
  type AttachableEngine,
  type CaptureState,
  type PrivacyMode,
} from '../capture/capture'
import { EventQueue, openQueueStorage } from '../capture/queue'
import { ensureIdentity } from '../capture/identity'
import { isStandalone } from '../standalone'
import {
  api,
  authToken,
  setAuthRecovery,
  setAuthToken,
  type ConsentRecord,
} from '../capture/api'

const CONSENT_KEY = 'gridline.consent'
const ACTOR_KEY = 'gridline.actor'
const WORKBOOK_KEY = 'gridline.workbook'
const SALT_KEY = 'gridline.salt'

function readLocal(key: string): string | null {
  try {
    return globalThis.localStorage?.getItem(key) ?? null
  } catch {
    return null
  }
}

function writeLocal(key: string, value: string): void {
  try {
    globalThis.localStorage?.setItem(key, value)
  } catch {
    // Storage disabled. Capture still works; it just forgets across reloads.
  }
}

function readOrCreate(key: string, make: () => string): string {
  const existing = readLocal(key)
  if (existing) return existing
  const made = make()
  writeLocal(key, made)
  return made
}

/**
 * A record only counts as consent when it names a mode the user could have
 * chosen. The server answers `GET /v1/consent/me` with a null mode when it has
 * nothing on file, and treating that as an answer would silently skip the
 * notice — the one failure mode this whole feature exists to prevent.
 */
function validConsent(record: ConsentRecord | null | undefined): ConsentRecord | null {
  if (!record) return null
  const mode = record.mode
  if (mode !== 'full' && mode !== 'structural' && mode !== 'off') return null
  return record
}

export function readConsent(): ConsentRecord | null {
  const raw = readLocal(CONSENT_KEY)
  if (!raw) return null
  try {
    return validConsent(JSON.parse(raw) as ConsentRecord)
  } catch {
    return null
  }
}

function writeConsent(record: ConsentRecord): void {
  writeLocal(CONSENT_KEY, JSON.stringify(record))
}

/**
 * Development overrides, read once at start-up and only in a dev build.
 *
 * `scripts/dev.sh` and `scripts/demo.sh` mint a user and a scripted history
 * on the server, and there is no sign-in screen: without this the app runs
 * unauthenticated and every request comes back 401. `VITE_DEV_WORKBOOK_ID`
 * exists for the demo, whose seeded routines belong to a workbook the client
 * would otherwise never name — a routines panel that cannot show the
 * routines that were mined for it is not a demo.
 *
 * Both are guarded on `import.meta.env.DEV`, so a production bundle can carry
 * neither a baked-in credential nor someone else's workbook.
 */
function devEnv(name: string): string | null {
  const env = import.meta.env as Record<string, string | boolean | undefined>
  if (!env?.DEV) return null
  const value = env[name]
  return typeof value === 'string' && value.length > 0 ? value : null
}

/** An opaque id: not an email address, not a name. See `docs/PRIVACY.md`. */
function opaqueId(prefix: string): string {
  return `${prefix}_${ulid().slice(-8).toLowerCase()}`
}

interface Pipeline {
  controller: CaptureController
  queue: EventQueue
}

/** Read lazily by the controller so the caller never has to push updates. */
const currentContext = { sheet: 'Sheet1', selection: 'A1' }

let pipeline: Pipeline | null = null

function getPipeline(): Pipeline {
  if (pipeline) return pipeline

  const queue = new EventQueue({ send: api.sender() })
  // IndexedDB opens asynchronously and may not open at all. Until it does the
  // queue is memory-backed and fully functional; adopting the store later
  // hands it the backlog accumulated in the meantime.
  void openQueueStorage()
    .then((storage) => queue.useStorage(storage))
    .catch(() => {})

  // A dev token is adopted only when nothing is stored, so a token typed in
  // by hand always wins over one baked into the dev server's environment.
  const devToken = devEnv('VITE_DEV_TOKEN')
  if (devToken && !authToken()) setAuthToken(devToken)

  // …but only while it works. A stored token the server rejects is dead, and
  // "the hand-typed one wins" must not mean "wins forever": re-seeding a
  // development database left the browser presenting a token that database
  // had never heard of, with no reload, restart or re-seed able to recover.
  // On a 401 the dev token replaces it and the request is tried once more.
  // Production builds have no dev token, so this does nothing there.
  setAuthRecovery(() => {
    const fresh = devEnv('VITE_DEV_TOKEN')
    if (!fresh || authToken() === fresh) return false
    setAuthToken(fresh)
    return true
  })

  const consent = readConsent()
  const workbookId = readOrCreate(
    WORKBOOK_KEY,
    () => devEnv('VITE_DEV_WORKBOOK_ID') ?? opaqueId('wb'),
  )
  const actorId = readOrCreate(ACTOR_KEY, () => opaqueId('u'))
  // The salt belongs to the server (`docs/PRIVACY.md`), and is adopted from
  // the consent response when there is one. Until then a locally generated
  // per-workbook salt keeps `structural` genuinely structural rather than
  // silently degrading to plaintext.
  const salt = readOrCreate(`${SALT_KEY}.${workbookId}`, () => `${ulid()}${ulid()}`)

  const controller = new CaptureController({
    actorId,
    workbookId,
    salt,
    context: () => currentContext,
    sink: queue,
    // Consent recorded in an earlier session is honoured without re-recording
    // a grant; a first run starts at `off` and stays there until the user says
    // otherwise.
    mode: consent?.mode ?? 'off',
  })

  pipeline = { controller, queue }
  return pipeline
}

export interface CaptureApi {
  state: CaptureState
  mode: PrivacyMode
  /** Events the ring buffer had to discard. Surfaced, never swallowed. */
  dropped: number
  /** Envelopes accepted but not yet acknowledged by the server. */
  pending: number
  /**
   * Envelopes the server refused outright — a bad token, a malformed batch.
   *
   * Distinct from `pending`, and the more urgent of the two: a queue that is
   * backing up will drain when the server comes back, while a rejected one
   * never will. Any number above zero means capture is not working.
   */
  rejected: number
  /**
   * Why this browser has no usable credential, or null when it has one.
   *
   * Separate from `rejected`, which counts batches the server refused. This
   * fires earlier and is more fundamental: without an account there is
   * nothing to refuse, and the queue would fill up behind a wall.
   */
  registration: string | null
  /** True until the user has answered the consent notice. */
  needsConsent: boolean
  /** Whether the backlog would survive a reload. */
  durable: boolean
  toggle: () => void
  choose: (mode: PrivacyMode) => void
  controller: CaptureController
}

export interface CaptureInput {
  engine: AttachableEngine | null
  sheet: string
  selection: string
}

export function useCapture({ engine, sheet, selection }: CaptureInput): CaptureApi {
  const { controller, queue } = getPipeline()
  currentContext.sheet = sheet
  currentContext.selection = selection

  const [stats, setStats] = useState(() => controller.stats())
  const [queueState, setQueueState] = useState(() => queue.state())
  const [consent, setConsent] = useState<ConsentRecord | null>(() => readConsent())
  const [registration, setRegistration] = useState<string | null>(null)

  useEffect(() => controller.subscribe(setStats), [controller])
  useEffect(() => queue.subscribe(setQueueState), [queue])

  // The capture hook. Everything downstream of this is bookkeeping.
  useEffect(() => {
    if (!engine) return
    return controller.attach(engine)
  }, [engine, controller])

  // Selection changes are sampled inside the controller; this just tells it
  // where the cursor went.
  useEffect(() => {
    controller.noteSelection(selection)
  }, [controller, selection, sheet])

  // Get the tail of a session out before the tab goes away.
  useEffect(() => {
    const flush = () => {
      void controller.flushNow()
    }
    const onVisibility = () => {
      if (document.visibilityState === 'hidden') flush()
    }
    window.addEventListener('pagehide', flush)
    document.addEventListener('visibilitychange', onVisibility)
    return () => {
      window.removeEventListener('pagehide', flush)
      document.removeEventListener('visibilitychange', onVisibility)
    }
  }, [controller])

  // Get a credential, then reconcile with the server's record.
  //
  // On a hosted deployment a first-time visitor has no token, so this is also
  // where the anonymous account is created. Registering is not consent: the
  // account arrives with no consent record, which is exactly what makes the
  // notice appear. A standalone build has no server at all, so it skips both
  // and stays at `off`.
  useEffect(() => {
    if (isStandalone()) return
    let cancelled = false
    void ensureIdentity().then((identity) => {
      if (cancelled) return
      if (identity.kind !== 'ready') {
        // Surfaced rather than logged. Without a credential nothing can be
        // captured, and a chip reading "capturing" would be a lie.
        setRegistration(identity.reason)
        return
      }
      setRegistration(null)
      if (identity.actorId) {
        writeLocal(ACTOR_KEY, identity.actorId)
        controller.setActorId(identity.actorId)
      }
      return reconcileConsent()
    })

    async function reconcileConsent() {
      const res = await api.getConsent()
      if (cancelled || !res.ok) return
      // The server tells us who it thinks we are; adopt that before sending
      // anything, or every envelope is rejected as a mismatched actor.
      const serverActor = (res.data as { actor_id?: string } | undefined)?.actor_id
      if (serverActor) {
        writeLocal(ACTOR_KEY, serverActor)
        controller.setActorId(serverActor)
      }
      const record = validConsent(res.data)
      // No record on file means the notice has not been answered. Leave the
      // modal showing rather than inventing an answer on the user's behalf.
      if (!record) return
      writeConsent(record)
      setConsent(record)
      if (record.salt) controller.setSalt(record.salt)
      controller.setMode(record.mode, record.consent_text_version)
    }

    return () => {
      cancelled = true
    }
  }, [controller])

  const choose = useCallback(
    (mode: PrivacyMode) => {
      const record: ConsentRecord = {
        mode,
        consent_text_version: CONSENT_TEXT_VERSION,
        granted_at_ms: Date.now(),
      }
      writeConsent(record)
      setConsent(record)
      controller.setMode(mode, CONSENT_TEXT_VERSION)
      // Best effort, and deliberately not awaited: the user's choice takes
      // effect locally the instant they make it, server or no server.
      void api.postConsent(record).then((res) => {
        if (res.ok && res.data?.salt) controller.setSalt(res.data.salt)
      })
    },
    [controller],
  )

  const toggle = useCallback(() => {
    controller.toggle()
  }, [controller])

  return {
    state: stats.state,
    mode: stats.mode,
    dropped: stats.dropped,
    pending: queueState.pending,
    // Batches the server refused outright. Surfaced rather than kept as an
    // internal counter: a permanent rejection means capture is not working at
    // all, and "capturing, 0 waiting" is a worse lie than any error message.
    rejected: queueState.discarded,
    registration,
    needsConsent: consent === null,
    durable: queueState.durable,
    toggle,
    choose,
    controller,
  }
}
