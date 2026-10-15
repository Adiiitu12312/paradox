import type { Commit } from './types'

export interface GraphRow {
  lane: number
  incoming: boolean
  edges: { from: number; to: number }[]
}

/** Lay out a bounded native Git page without duplicating pending ancestors. */
export function deriveGraph(commits: Commit[]): { rows: GraphRow[]; width: number } {
  const pending: (string | undefined)[] = []
  let widestLane = 0
  const freeLane = () => {
    const available = pending.indexOf(undefined)
    return available < 0 ? pending.length : available
  }
  const rows = commits.map(commit => {
    const existing = pending.indexOf(commit.sha)
    const incoming = existing >= 0
    const lane = incoming ? existing : freeLane()
    const edges = pending.flatMap((sha, index) =>
      sha !== undefined && index !== lane ? [{ from: index, to: index }] : [])
    // Consume only this commit's slot. Other lanes never shift when a branch
    // ends or joins an ancestor already represented elsewhere.
    pending[lane] = undefined
    for (const [index, parent] of [...new Set(commit.parents)].entries()) {
      let destination = pending.indexOf(parent)
      if (destination < 0) {
        destination = index === 0 && pending[lane] === undefined ? lane : freeLane()
        pending[destination] = parent
      }
      edges.push({ from: lane, to: destination })
    }
    widestLane = Math.max(widestLane, lane, pending.length - 1)
    while (pending.length && pending[pending.length - 1] === undefined) pending.pop()
    return { lane, incoming, edges }
  })
  // Node centers are 12 + lane*13; retain twelve pixels on either side.
  return { rows, width: 24 + widestLane * 13 }
}
