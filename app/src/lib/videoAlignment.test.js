import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import {
  activeVideoAt,
  exportSegments,
  formatClockOffset,
  sharedClockOffsetSec,
  videoStartOnAxis,
} from './videoAlignment.js'

const GPX = '2026-11-14T02:07:26Z'
// Camera clock 1228 s fast, corrected by a shared offset.
const clip = (name, creationTime, duration, extra = {}) => ({
  id: name,
  path: `/v/${name}.MP4`,
  creationTime,
  duration,
  userOffsetSec: -1228,
  missing: false,
  ...extra,
})
const A = clip('A', '2026-11-14T02:32:18Z', 1846.7)
const B = clip('B', '2026-11-14T03:03:16Z', 3)
const C = clip('C', '2026-11-14T03:05:04Z', 59.3)

describe('videoStartOnAxis with a shared camera offset', () => {
  it('applies the correction on top of wall-clock placement', () => {
    // 02:32:18 − 02:07:26 = 1492 s, minus 1228 s fast clock.
    assert.equal(videoStartOnAxis(GPX, A), 264)
  })
})

describe('activeVideoAt', () => {
  it('finds the clip covering the playhead', () => {
    const videos = [A, B, C]
    assert.equal(activeVideoAt(GPX, videos, 300)?.id, 'A')
    const bStart = videoStartOnAxis(GPX, B)
    assert.equal(activeVideoAt(GPX, videos, bStart + 1)?.id, 'B')
  })
  it('returns null in gaps and skips missing files', () => {
    assert.equal(activeVideoAt(GPX, [A, B, C], 10), null)
    const gone = { ...A, missing: true }
    assert.equal(activeVideoAt(GPX, [gone], 300), null)
  })
  it('prefers the most recently started clip when clips overlap', () => {
    const late = { ...A, id: 'late', userOffsetSec: -1128 }
    assert.equal(activeVideoAt(GPX, [A, late], 500)?.id, 'late')
  })
})

describe('exportSegments', () => {
  it('orders clips by time and trims them to the export window', () => {
    const aStart = videoStartOnAxis(GPX, A)
    const segs = exportSegments(GPX, [C, A, B], aStart + 100, 1e9)
    assert.deepEqual(
      segs.map((s) => s.videoPath),
      ['/v/A.MP4', '/v/B.MP4', '/v/C.MP4'],
    )
    assert.equal(segs[0].videoIn, 100)
    assert.equal(segs[0].start, aStart + 100)
    assert.ok(Math.abs(segs[0].end - (aStart + 1846.7)) < 1e-9)
    assert.equal(segs[1].videoIn, 0)
  })
  it('drops excluded clips and slivers', () => {
    const bStart = videoStartOnAxis(GPX, B)
    const segs = exportSegments(
      GPX,
      [A, { ...C, excluded: true }, B],
      bStart + 2.8,
      1e9,
    )
    assert.deepEqual(segs, [])
  })
})

describe('sharedClockOffsetSec', () => {
  it('reports the common correction', () => {
    assert.equal(sharedClockOffsetSec([A, B, C]), -1228)
  })
  it('is null when clips disagree or have no timestamps', () => {
    assert.equal(sharedClockOffsetSec([A, { ...B, userOffsetSec: 0 }]), null)
    assert.equal(sharedClockOffsetSec([{ ...A, creationTime: null }]), null)
  })
})

describe('formatClockOffset', () => {
  it('formats signed offsets', () => {
    assert.equal(formatClockOffset(-1228), '−20m 28s')
    assert.equal(formatClockOffset(42), '+42s')
    assert.equal(formatClockOffset(-15600), '−4h 20m 0s')
  })
})
