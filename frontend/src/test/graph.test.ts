import { describe, expect, it } from 'vitest'
import { deriveGraph } from '../graph'
import type { Commit } from '../types'

function commit(sha: string, ...parents: string[]): Commit {
  return { sha, parents, subject: sha, author_name: 'Fixture', author_email: 'fixture@example.invalid', time: 0, refs: [] }
}

describe('commit graph lanes', () => {
  it('joins a diamond onto its existing ancestor lane without a ghost track', () => {
    const graph = deriveGraph([commit('merge', 'left', 'right'), commit('left', 'root'), commit('right', 'root'), commit('root')])
    expect(graph.rows.map(row => row.lane)).toEqual([0, 0, 1, 0])
    expect(graph.rows.map(row => row.incoming)).toEqual([false, true, true, true])
    expect(graph.rows[2].edges).toEqual([{ from: 0, to: 0 }, { from: 1, to: 0 }])
    expect(graph.rows[3].edges).toEqual([])
    expect(graph.width).toBe(37)
  })

  it('does not draw an incoming edge for unrelated roots or newly encountered branches', () => {
    const graph = deriveGraph([commit('first', 'ancestor'), commit('other-root'), commit('ancestor'), commit('last-root')])
    expect(graph.rows.map(row => [row.lane, row.incoming])).toEqual([[0, false], [1, false], [0, true], [0, false]])
    expect(graph.rows[1].edges).toEqual([{ from: 0, to: 0 }])
    expect(graph.rows[3].edges).toEqual([])
  })

  it('reuses freed slots while preserving occupied lane positions', () => {
    const graph = deriveGraph([commit('merge', 'left', 'middle', 'right'), commit('middle'), commit('new', 'new-parent'), commit('left'), commit('right'), commit('new-parent')])
    expect(graph.rows.map(row => row.lane)).toEqual([0, 1, 1, 0, 2, 1])
    expect(graph.rows.map(row => row.incoming)).toEqual([false, true, false, true, true, true])
    expect(graph.rows.at(-1)?.edges).toEqual([])
  })

  it('fits wide merges and converges every child onto one shared parent track', () => {
    const children = Array.from({ length: 9 }, (_, index) => `child-${index}`)
    const graph = deriveGraph([commit('octopus', ...children), ...children.map(sha => commit(sha, 'root')), commit('root')])
    expect(graph.width).toBe(24 + 8 * 13)
    expect(graph.rows[0].edges).toHaveLength(9)
    expect(graph.rows.at(-1)).toEqual({ lane: 0, incoming: true, edges: [] })
    for (const row of graph.rows) {
      const continuationLanes = row.edges.filter(edge => edge.from !== row.lane).map(edge => edge.to)
      expect(new Set(continuationLanes).size).toBe(continuationLanes.length)
      for (const edge of row.edges) expect(12 + edge.to * 13).toBeLessThan(graph.width)
    }
  })

  it('keeps a first-parent continuation in its lane when an earlier slot is free', () => {
    const graph = deriveGraph([commit('merge', 'left', 'middle', 'right'), commit('left'), commit('right', 'right-parent'), commit('middle'), commit('right-parent')])
    expect(graph.rows.map(row => row.lane)).toEqual([0, 0, 2, 1, 2])
    expect(graph.rows[2].edges).toEqual([{ from: 1, to: 1 }, { from: 2, to: 2 }])
    expect(graph.rows.at(-1)?.edges).toEqual([])
  })
})
