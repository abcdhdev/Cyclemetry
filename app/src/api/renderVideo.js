import * as backend from './backend.js'
import { exportBitsPerPixelSecond } from '../lib/utils.js'
import { exportSegments, playableVideos } from '../lib/videoAlignment.js'

const ETA_REANCHOR_MS = 30_000
const ETA_MIN_SAMPLE_SECONDS = 5
const ETA_MAX_UPWARD_ADJUST_SECONDS = 15

export default async function renderVideo(state) {
  const { gpxFilename, outputDir, outputWidth, outputHeight, exportFormat } =
    state
  // renderConfig = the working template plus app-level settings (unit system)
  // that are injected at render time but never saved into the template.
  const config = state.renderConfig
  if (!config?.scene) throw new Error('No valid config available')
  if (!gpxFilename) throw new Error('No GPX file selected')
  const start = config.scene.start ?? 0
  const end = config.scene.end ?? state.timelineDuration
  if (start >= end)
    throw new Error(
      `Timeline start (${start}s) must be less than end (${end}s) — fix the timeline range before rendering`,
    )

  const fps = config.scene.fps ?? 30

  // Stitched export: composite the overlay onto the footage. Every included
  // clip that overlaps the scene range becomes one segment, trimmed to the
  // overlap; with several clips they're joined back to back into one MP4,
  // each carrying the overlay for its own stretch of the ride.
  let renderConfig = config
  let renderStart = start
  let renderEnd = end
  let stitch = null
  let exportSeconds = end - start
  if (exportFormat === 'stitched') {
    const segments = exportSegments(
      state.gpxStartTime,
      state.videos,
      start,
      end,
    )
    if (segments.length === 0)
      throw new Error(
        playableVideos(state.videos).length === 0
          ? 'Stitched export needs a video — load one first'
          : 'The export range does not overlap any included video — adjust the timeline range or move the videos on the timeline',
      )
    renderStart = segments[0].start
    renderEnd = segments[segments.length - 1].end
    exportSeconds = segments.reduce((sum, s) => sum + (s.end - s.start), 0)
    renderConfig = {
      ...config,
      scene: { ...config.scene, start: renderStart, end: renderEnd },
    }
    stitch =
      segments.length === 1
        ? { videoPath: segments[0].videoPath, videoIn: segments[0].videoIn }
        : { segments }
  }

  state.renderProgress = {
    current: 0,
    total: 0,
    percent: 0,
    status: 'starting',
    estimatedSecondsRemaining: null,
    overlaySecondsRendered: 0,
    overlayTotalSeconds: 0,
  }
  state.renderingVideo = true

  try {
    const startData = await backend.nativeStartRender(
      renderConfig,
      gpxFilename,
      outputDir,
      outputWidth,
      outputHeight,
      exportFormat,
      stitch,
      state.riderWeightKg,
      // Full-frame only applies to transparent overlays; stitched always
      // composites to full canvas, so force it off there.
      exportFormat === 'stitched' ? false : state.exportFullFrame,
    )
    const outputPath = startData.output_path

    await new Promise((resolve, reject) => {
      const t0 = Date.now()
      let etaAnchorRemaining = null
      let etaAnchorAt = null
      let etaLastReanchorAt = 0

      function currentEta(now) {
        if (etaAnchorRemaining == null || etaAnchorAt == null) return null
        return Math.max(
          0,
          Math.round(etaAnchorRemaining - (now - etaAnchorAt) / 1000),
        )
      }

      function reanchorEta(rawRemaining, now) {
        const shown = currentEta(now)
        let next = rawRemaining

        if (shown != null) {
          if (rawRemaining > shown) {
            next = Math.min(
              shown + ETA_MAX_UPWARD_ADJUST_SECONDS,
              Math.round(shown * 0.8 + rawRemaining * 0.2),
            )
          } else {
            next = Math.round(shown * 0.35 + rawRemaining * 0.65)
          }
        }

        etaAnchorRemaining = next
        etaAnchorAt = now
        etaLastReanchorAt = now
      }

      const iv = setInterval(async () => {
        try {
          const now = Date.now()
          const p = await backend.nativeGetProgress()
          const elapsed = (now - t0) / 1000
          const rate = elapsed > 0 ? p.frames_rendered / elapsed : 0
          const rawRemaining =
            elapsed >= ETA_MIN_SAMPLE_SECONDS && rate > 0
              ? Math.round((p.total_frames - p.frames_rendered) / rate)
              : null
          // While paused for low disk, elapsed time keeps growing with no
          // frames rendered, so the measured rate is garbage — hold the ETA
          // anchor and let it re-establish after the render resumes.
          if (
            rawRemaining != null &&
            !p.paused_low_disk &&
            (etaAnchorRemaining == null ||
              now - etaLastReanchorAt >= ETA_REANCHOR_MS)
          ) {
            reanchorEta(rawRemaining, now)
          }
          const pct =
            p.total_frames > 0
              ? Math.round((p.frames_rendered / p.total_frames) * 100)
              : 0
          state.renderProgress = {
            current: p.frames_rendered,
            total: p.total_frames,
            percent: pct,
            status: p.paused_low_disk ? 'paused-low-disk' : 'rendering',
            estimatedSecondsRemaining: currentEta(now),
            // Overlay timeline position (NOT wall-clock): how much of the
            // overlay's own duration has been rendered, so the user can sanity
            // -check the render length while it runs.
            overlaySecondsRendered: p.frames_rendered / fps,
            overlayTotalSeconds: p.total_frames / fps,
          }
          if (!p.is_running) {
            clearInterval(iv)
            if (p.error) reject(new Error(p.error))
            else {
              if (p.frames_rendered > 0 && elapsed > 0) {
                state.recordRenderFps(exportFormat, p.frames_rendered / elapsed)
              }
              resolve()
            }
          }
        } catch (err) {
          clearInterval(iv)
          reject(err)
        }
      }, 200)
    })

    try {
      const info = await backend.fileSize(outputPath)
      const bitsPerPixelSecond = exportBitsPerPixelSecond(
        info.bytes,
        outputWidth,
        outputHeight,
        fps,
        exportSeconds,
      )
      state.recordExportSizeEstimate(bitsPerPixelSecond, exportFormat)
    } catch {
      /* file-size calibration is best-effort */
    }

    try {
      await backend.openVideo(outputPath)
    } catch {
      /* open is best-effort */
    }
    return { success: true, filename: outputPath }
  } catch (error) {
    if (error.message?.toLowerCase().includes('cancelled')) {
      return { success: false, cancelled: true }
    }
    throw error
  } finally {
    state.renderingVideo = false
    state.renderProgress = {
      current: 0,
      total: 0,
      percent: 0,
      status: 'idle',
      estimatedSecondsRemaining: null,
      overlaySecondsRendered: 0,
      overlayTotalSeconds: 0,
    }
  }
}
