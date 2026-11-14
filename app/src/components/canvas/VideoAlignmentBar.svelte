<script>
  /**
   * Shows every reference clip's extent on the same time axis as the
   * playback scrubber. Bands are draggable to correct the camera clock.
   *
   * Clips from one camera share its clock error, so dragging a band moves
   * all clips together; hold Alt (Option) to move just that clip. Auto-sync
   * finds the shared correction from GoPro motion data instead.
   *
   * Bands are positioned by the shared `videoStartOnAxis()` helper — when
   * both timestamps are present this is real wall-clock alignment; otherwise
   * the offset alone places it.
   *
   * Auto-snap (single clip only): if the band is entirely outside the
   * overlay window, it is moved to the timeline start.
   */
  import { getContext } from 'svelte'
  import { LoaderCircle, Waves } from 'lucide-svelte'
  import {
    activeVideoAt,
    formatClockOffset,
    offsetForVideoStart,
    playableVideos,
    sharedClockOffsetSec,
    videoStartOnAxis as computeStartOnAxis,
    wallClockDeltaSec,
  } from '@/lib/videoAlignment.js'

  const app = getContext('app')

  let trackEl = $state(null)
  let drag = $state(null)

  let clips = $derived(playableVideos(app.videos))
  let activeId = $derived(
    activeVideoAt(app.gpxStartTime, app.videos, app.selectedSecond)?.id ?? null,
  )
  let clockOffset = $derived(sharedClockOffsetSec(app.videos))
  let canAutoSync = $derived(clips.some((v) => v.hasMotionData))

  // The bar's x-axis mirrors the playback scrub bar: left = sceneStart,
  // right = sceneEnd. As the overlay window shrinks the visible portion
  // of each band grows proportionally.
  let sceneStart = $derived(app.config?.scene?.start ?? 0)
  let sceneEnd = $derived(app.config?.scene?.end ?? app.timelineDuration ?? 0)
  let overlayDuration = $derived(Math.max(0.0001, sceneEnd - sceneStart))

  let bands = $derived(
    clips.map((v) => {
      const startAbs = computeStartOnAxis(app.gpxStartTime, v)
      const startRel = startAbs - sceneStart
      const endRel = startRel + v.duration
      const leftPct = Math.max(0, (startRel / overlayDuration) * 100)
      const rightPct = Math.min(100, (endRel / overlayDuration) * 100)
      return {
        v,
        startAbs,
        leftPct,
        widthPct: Math.max(0, rightPct - leftPct),
        overflowLeft: startRel < 0,
        overflowRight: endRel > overlayDuration,
        overlaps:
          Math.min(overlayDuration, endRel) > Math.max(0, startRel) &&
          v.duration > 0,
      }
    }),
  )
  let anyOverflowLeft = $derived(bands.some((b) => b.overflowLeft))
  let anyOverflowRight = $derived(bands.some((b) => b.overflowRight))

  function clamp(v, lo, hi) {
    return Math.min(hi, Math.max(lo, v))
  }

  function basename(path) {
    return path?.split(/[\\/]/).pop() ?? ''
  }

  function beginDrag(e, band) {
    if (!trackEl) return
    e.preventDefault()
    trackEl.setPointerCapture(e.pointerId)
    app.beginEditBatch?.()
    const solo = e.altKey || clips.length === 1
    const moving = solo ? [band.v] : clips
    drag = {
      pointerId: e.pointerId,
      startX: e.clientX,
      grabbed: band.v,
      grabbedStart: band.startAbs,
      // Snapshot so the whole drag is applied from its starting point —
      // incremental updates would accumulate rounding drift.
      initial: Object.fromEntries(
        moving.map((v) => [v.id, computeStartOnAxis(app.gpxStartTime, v)]),
      ),
      solo,
    }
  }

  function onPointerMove(e) {
    if (!drag || e.pointerId !== drag.pointerId) return
    const w = trackEl?.offsetWidth ?? 1
    // The bar's pixel width represents `overlayDuration` seconds — convert
    // drag deltas in that scale, so a 10% drag shifts by 10% of the window.
    const dxSec = ((e.clientX - drag.startX) / w) * overlayDuration
    // Keep the grabbed band's centre inside the visible window so it always
    // stays grabbable (bounding the offset itself would destroy large
    // wall-clock-derived offsets).
    const dur = drag.grabbed.duration
    const target = clamp(
      drag.grabbedStart + dxSec,
      sceneStart - dur / 2,
      sceneEnd - dur / 2,
    )
    const delta = target - drag.grabbedStart
    const next = {}
    for (const v of clips) {
      if (!(v.id in drag.initial)) continue
      next[v.id] = offsetForVideoStart(
        app.gpxStartTime,
        v,
        drag.initial[v.id] + delta,
      )
    }
    app.setVideoOffsets(next)
  }

  function endDrag(e) {
    if (!drag) return
    if (trackEl?.hasPointerCapture(e.pointerId)) {
      trackEl.releasePointerCapture(e.pointerId)
    }
    app.endEditBatch?.()
    drag = null
  }

  // ── Single-clip auto-snap (legacy behaviour) ─────────────────────────────
  let single = $derived(clips.length === 1 ? bands[0] : null)
  // Deliberate wall-clock placement: userOffsetSec 0 with both timestamps
  // known means the band sits exactly at the camera's recorded position
  // (the "align to recording time" button). Auto-snap must never override it.
  let wallClockAligned = $derived(
    !!single &&
      (single.v.userOffsetSec ?? 0) === 0 &&
      wallClockDeltaSec(app.gpxStartTime, single.v) != null,
  )
  $effect(() => {
    if (single && !single.overlaps && !drag && !wallClockAligned) {
      app.setVideoOffset(
        single.v.id,
        offsetForVideoStart(app.gpxStartTime, single.v, sceneStart),
      )
    }
  })
</script>

{#if clips.length > 0}
  <div class="px-4 pt-2 pb-1 space-y-1.5">
    <div class="flex items-center gap-3 text-[10px]">
      <span class="font-semibold uppercase tracking-wider text-sky-400/80"
        >Video alignment</span
      >
      {#if clips.length > 1}
        <span class="text-[var(--dim)]"
          >{clips.length} clips · Alt-drag moves one</span
        >
      {/if}
      {#if clockOffset != null && clockOffset !== 0}
        <span
          class="font-mono tabular-nums text-[var(--muted-foreground)]"
          title="Correction applied to the camera's recording time"
          >Camera clock {formatClockOffset(-clockOffset)}</span
        >
      {/if}
      {#if canAutoSync}
        <button
          type="button"
          onclick={() => app.autoSyncVideos()}
          disabled={app.syncingVideos}
          class="ml-auto inline-flex items-center gap-1 rounded-[6px] px-1.5 py-0.5 text-sky-400/90 hover:bg-[var(--panel2)] hover:text-sky-300 cursor-pointer disabled:cursor-wait disabled:opacity-60"
          title="Match handlebar vibration in the GoPro telemetry to the activity speed and fix the camera clock for all clips"
        >
          {#if app.syncingVideos}
            <LoaderCircle size={11} class="animate-spin" /> Syncing…
          {:else}
            <Waves size={11} /> Auto-sync
          {/if}
        </button>
      {/if}
    </div>

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      bind:this={trackEl}
      class="relative h-5 select-none"
      onpointermove={onPointerMove}
      onpointerup={endDrag}
      onpointercancel={endDrag}
    >
      <div
        class="absolute inset-x-0 top-2 h-1 rounded-full bg-[var(--panel3)]"
      ></div>
      {#each bands as b (b.v.id)}
        {#if b.widthPct > 0}
          <button
            type="button"
            aria-label="{basename(b.v.path)} — drag to nudge the offset"
            title="{basename(b.v.path)}{b.v.excluded
              ? ' (not exported)'
              : ''} — drag to move{clips.length > 1
              ? ' all clips, Alt-drag to move only this one'
              : ''}"
            onpointerdown={(e) => beginDrag(e, b)}
            class="absolute top-1 h-3 min-w-[3px] rounded-[2px] border transition-colors cursor-grab
                   {b.v.excluded
              ? 'border-dashed border-sky-400/40 bg-sky-500/15 hover:bg-sky-500/30'
              : b.v.id === activeId
                ? 'border-sky-300 bg-sky-400/80 hover:bg-sky-400/90'
                : 'border-sky-400/80 bg-sky-500/60 hover:bg-sky-500/80'}
                   {drag ? 'cursor-grabbing' : ''}"
            style="left: {b.leftPct}%; width: {b.widthPct}%"
          ></button>
        {/if}
      {/each}
      {#if anyOverflowLeft}
        <span
          class="absolute top-1 left-0 text-sky-400 text-[10px] leading-none pointer-events-none"
          title="Footage starts before the overlay window">‹</span
        >
      {/if}
      {#if anyOverflowRight}
        <span
          class="absolute top-1 right-0 text-sky-400 text-[10px] leading-none pointer-events-none"
          title="Footage continues after the overlay window">›</span
        >
      {/if}
    </div>
  </div>
{/if}
