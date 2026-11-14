/**
 * Wall-clock difference between the video's first frame and the GPX origin,
 * in seconds (positive = video starts after the GPX). Null when either
 * timestamp is missing or unparseable — callers use null to distinguish
 * "manual placement only" from a real wall-clock relationship.
 */
export function wallClockDeltaSec(gpxStartTime, video) {
  const gpxMs = gpxStartTime ? Date.parse(gpxStartTime) : NaN
  const videoMs = video?.creationTime ? Date.parse(video.creationTime) : NaN
  if (isNaN(gpxMs) || isNaN(videoMs)) return null
  return (videoMs - gpxMs) / 1000
}

/**
 * Where the reference video's first frame sits on the GPX time axis,
 * in seconds from the GPX origin.
 *
 * When both `gpxStartTime` (ISO 8601) and `video.creationTime` (ISO 8601)
 * are present, the difference gives us real wall-clock alignment. The
 * `userOffsetSec` then compensates for camera clock drift or wrong-timezone
 * recordings.
 *
 * When either timestamp is missing, the offset alone places the video on
 * the axis — the UI treats this as "manual placement" mode.
 */
export function videoStartOnAxis(gpxStartTime, video) {
  if (!video) return 0
  const userOffset = video.userOffsetSec ?? 0
  const delta = wallClockDeltaSec(gpxStartTime, video)
  return delta != null ? delta + userOffset : userOffset
}

/**
 * Compute the userOffsetSec that puts the video's first frame at
 * `targetSec` on the GPX time axis. Inverse of videoStartOnAxis.
 */
export function offsetForVideoStart(gpxStartTime, video, targetSec) {
  if (!video) return 0
  const delta = wallClockDeltaSec(gpxStartTime, video)
  return Math.round(delta != null ? targetSec - delta : targetSec)
}

/**
 * True when wall-clock alignment (userOffsetSec = 0) is meaningful AND
 * the resulting video extent overlaps the GPX activity. Decides whether
 * wall-clock placement should be trusted automatically.
 */
export function wallClockApplicable(gpxStartTime, video, activityDuration) {
  const startAtZero = wallClockDeltaSec(gpxStartTime, video)
  if (startAtZero == null) return false
  const endAtZero = startAtZero + (video.duration ?? 0)
  const dur = activityDuration ?? 0
  return endAtZero > 0 && startAtZero < dur
}

/** Clips that can be shown or exported: on disk and with a known duration. */
export function playableVideos(videos) {
  return (videos ?? []).filter(
    (v) => v?.path && !v.missing && (v.duration ?? 0) > 0,
  )
}

/**
 * The clip whose footage covers `second` on the GPX axis, or null. When
 * clips overlap (two cameras, or a clip nudged onto another), the one that
 * started most recently wins — it's the footage that "just cut in".
 */
export function activeVideoAt(gpxStartTime, videos, second) {
  let best = null
  let bestStart = -Infinity
  for (const v of playableVideos(videos)) {
    const start = videoStartOnAxis(gpxStartTime, v)
    if (second >= start && second <= start + v.duration && start > bestStart) {
      best = v
      bestStart = start
    }
  }
  return best
}

/**
 * Stitched-export plan: every included clip that overlaps the
 * `start`..`end` export window, in timeline order, trimmed to the window.
 * Each entry pairs a footage in-point with the activity window it covers.
 * Slivers shorter than half a second are dropped — they'd be a flash cut.
 */
export function exportSegments(gpxStartTime, videos, start, end) {
  return playableVideos(videos)
    .filter((v) => !v.excluded)
    .map((v) => {
      const vStart = videoStartOnAxis(gpxStartTime, v)
      const segStart = Math.max(start, vStart)
      const segEnd = Math.min(end, vStart + v.duration)
      return {
        videoPath: v.path,
        videoIn: segStart - vStart,
        start: segStart,
        end: segEnd,
      }
    })
    .filter((s) => s.end - s.start >= 0.5)
    .sort((a, b) => a.start - b.start)
}

/**
 * The camera clock correction shared by every clip placed by wall clock, in
 * seconds (negative = the camera clock ran fast). Null when no clip has a
 * timestamp or the clips disagree (moved individually).
 */
export function sharedClockOffsetSec(videos) {
  const timed = (videos ?? []).filter((v) => v?.creationTime)
  if (timed.length === 0) return null
  const first = timed[0].userOffsetSec ?? 0
  return timed.every((v) => (v.userOffsetSec ?? 0) === first) ? first : null
}

/** "−20m 28s" style label for a clock offset in seconds. */
export function formatClockOffset(sec) {
  const s = Math.round(Math.abs(sec))
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const r = s % 60
  const body = h > 0 ? `${h}h ${m}m ${r}s` : m > 0 ? `${m}m ${r}s` : `${r}s`
  return `${sec < 0 ? '−' : '+'}${body}`
}
