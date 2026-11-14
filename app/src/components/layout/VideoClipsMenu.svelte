<script>
  /**
   * Header control for the project's reference videos. With no videos it's
   * an "Add videos…" button; otherwise a chip (filename, or "N videos") that
   * opens a list of clips: include-in-export toggle, timeline position,
   * relink/remove, plus bulk actions — add more, align to camera recording
   * time, motion auto-sync, remove all.
   */
  import { getContext } from 'svelte'
  import {
    AlertTriangle,
    ChevronDown,
    Clock,
    Eye,
    EyeOff,
    Film,
    LoaderCircle,
    Plus,
    Waves,
    X,
  } from 'lucide-svelte'
  import Tooltip from '@/components/ui/Tooltip.svelte'
  import { TOOLTIP_DELAY } from '@/lib/utils.js'
  import {
    formatClockOffset,
    sharedClockOffsetSec,
    videoStartOnAxis,
    wallClockDeltaSec,
  } from '@/lib/videoAlignment.js'

  let { onalignrecordingtime } = $props()

  const app = getContext('app')

  let open = $state(false)

  let videos = $derived(app.videos ?? [])
  let missingCount = $derived(videos.filter((v) => v.missing).length)
  let canAutoSync = $derived(
    app.hasActivity && videos.some((v) => !v.missing && v.hasMotionData),
  )
  let canUseRecordingTime = $derived(
    app.hasActivity &&
      videos.some((v) => wallClockDeltaSec(app.gpxStartTime, v) != null),
  )
  let clockOffset = $derived(sharedClockOffsetSec(videos))

  function basename(path) {
    return path?.split(/[\\/]/).pop() ?? ''
  }

  let chipLabel = $derived(
    videos.length === 1 ? basename(videos[0].path) : `${videos.length} videos`,
  )

  // Position on the activity axis as h:mm:ss from the activity start.
  function axisLabel(sec) {
    const sign = sec < 0 ? '−' : ''
    const s = Math.round(Math.abs(sec))
    const h = Math.floor(s / 3600)
    const m = Math.floor((s % 3600) / 60)
    const r = s % 60
    const pad = (n) => String(n).padStart(2, '0')
    return `${sign}${h}:${pad(m)}:${pad(r)}`
  }

  function durationLabel(sec) {
    const s = Math.round(sec ?? 0)
    if (s < 60) return `${s}s`
    const m = Math.floor(s / 60)
    return m < 60 ? `${m}m ${s % 60}s` : `${Math.floor(m / 60)}h ${m % 60}m`
  }

  async function addVideos() {
    open = false
    await app.pickAndLoadVideo()
  }
</script>

<svelte:window
  onclick={() => {
    open = false
  }}
  onkeydown={(e) => {
    if (e.key === 'Escape') open = false
  }}
/>

{#if videos.length === 0}
  <Tooltip
    content="Add one or more ride videos — for preview, alignment, and the 'Video with overlay' export."
    side="bottom"
    class="shrink-0"
  >
    <button
      onclick={() => app.pickAndLoadVideo()}
      class="hdr-btn px-2.5 gap-1.5 border-dashed border-[var(--panel3)] bg-transparent hover:bg-[var(--panel2)] cursor-pointer"
    >
      <Film size={12} class="shrink-0" />
      Add videos…
    </button>
  </Tooltip>
{:else}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div class="relative shrink-0" onclick={(e) => e.stopPropagation()}>
    <button
      type="button"
      onclick={() => (open = !open)}
      class="hdr-btn px-2.5 gap-1.5 max-w-[190px] cursor-pointer {open
        ? 'bg-[var(--panel3)] text-zinc-100'
        : ''} {missingCount
        ? 'border-red-800/60 text-red-300 hover:border-red-600/60'
        : ''}"
      aria-haspopup="menu"
      aria-expanded={open}
      title={videos.map((v) => v.path).join('\n')}
    >
      {#if missingCount}
        <AlertTriangle size={12} class="text-red-400 shrink-0" />
      {:else}
        <Film size={12} class="text-zinc-500 shrink-0" />
      {/if}
      <span class="truncate text-zinc-200">{chipLabel}</span>
      <ChevronDown size={12} class="text-zinc-600 shrink-0" />
    </button>

    {#if open}
      <div
        class="absolute left-0 top-[calc(100%+6px)] z-[80] w-[340px] rounded-[10px] border border-white/[0.08] bg-[#181818] p-2 shadow-2xl shadow-black/60"
        role="menu"
      >
        <div
          class="flex items-center justify-between px-1.5 pb-1.5 text-[10px] uppercase tracking-wider text-[var(--muted-foreground)]"
        >
          <span>Videos</span>
          {#if clockOffset != null && clockOffset !== 0}
            <span class="font-mono normal-case tracking-normal text-zinc-400"
              >Camera clock {formatClockOffset(-clockOffset)}</span
            >
          {/if}
        </div>

        <div class="max-h-[300px] overflow-y-auto space-y-0.5">
          {#each videos as v (v.id)}
            {@const start = videoStartOnAxis(app.gpxStartTime, v)}
            <div
              class="group flex items-center gap-2 rounded-[6px] px-1.5 py-1 hover:bg-[var(--panel2)]"
            >
              <input
                type="checkbox"
                checked={!v.excluded}
                disabled={v.missing}
                onchange={(e) => app.setVideoExcluded(v.id, !e.target.checked)}
                class="h-3.5 w-3.5 shrink-0 accent-[#DC143C] cursor-pointer"
                title="Include in the 'Video with overlay' export"
                aria-label="Include {basename(v.path)} in export"
              />
              <div class="min-w-0 flex-1">
                <div
                  class="truncate text-xs {v.missing
                    ? 'text-red-300'
                    : v.excluded
                      ? 'text-zinc-500'
                      : 'text-zinc-200'}"
                  title={v.path}
                >
                  {basename(v.path)}
                </div>
                <div
                  class="font-mono text-[10px] tabular-nums text-[var(--dim)]"
                >
                  {#if v.missing}
                    File not found
                  {:else}
                    at {axisLabel(start)} · {durationLabel(v.duration)}
                  {/if}
                </div>
              </div>
              {#if v.missing}
                <button
                  type="button"
                  onclick={() => app.relinkVideo(v.id)}
                  class="text-[10px] text-sky-400 hover:text-sky-300 cursor-pointer"
                  >Locate…</button
                >
              {/if}
              <button
                type="button"
                onclick={() => app.removeVideo(v.id)}
                class="rounded p-0.5 text-zinc-600 opacity-0 group-hover:opacity-100 hover:text-zinc-200 cursor-pointer"
                aria-label="Remove {basename(v.path)}"
                title="Remove from project"><X size={12} /></button
              >
            </div>
          {/each}
        </div>

        <div
          class="mt-2 border-t border-white/[0.06] pt-2 grid grid-cols-2 gap-1"
        >
          <button
            type="button"
            onclick={addVideos}
            class="resolution-option cursor-pointer"
            role="menuitem"
          >
            <span class="inline-flex items-center gap-1.5 font-medium"
              ><Plus size={12} /> Add videos…</span
            >
            <span class="text-[10px] text-zinc-500">Select several at once</span
            >
          </button>
          <button
            type="button"
            disabled={!canAutoSync || app.syncingVideos}
            onclick={() => app.autoSyncVideos()}
            class="resolution-option cursor-pointer disabled:cursor-not-allowed disabled:opacity-40"
            role="menuitem"
            title={canAutoSync
              ? 'Match handlebar vibration in the GoPro telemetry to the activity speed and fix the camera clock for all clips'
              : 'Needs an activity and GoPro footage with telemetry'}
          >
            <span class="inline-flex items-center gap-1.5 font-medium">
              {#if app.syncingVideos}
                <LoaderCircle size={12} class="animate-spin" /> Syncing…
              {:else}
                <Waves size={12} /> Auto-sync
              {/if}
            </span>
            <span class="text-[10px] text-zinc-500">From motion (GoPro)</span>
          </button>
          <button
            type="button"
            disabled={!canUseRecordingTime}
            onclick={() => {
              onalignrecordingtime?.()
            }}
            class="resolution-option cursor-pointer disabled:cursor-not-allowed disabled:opacity-40"
            role="menuitem"
          >
            <span class="inline-flex items-center gap-1.5 font-medium"
              ><Clock size={12} /> Recording time</span
            >
            <span class="text-[10px] text-zinc-500">Trust the camera clock</span
            >
          </button>
          <button
            type="button"
            onclick={() => {
              open = false
              app.clearVideos()
            }}
            class="resolution-option cursor-pointer"
            role="menuitem"
          >
            <span class="inline-flex items-center gap-1.5 font-medium"
              ><X size={12} /> Remove all</span
            >
            <span class="text-[10px] text-zinc-500">Files stay on disk</span>
          </button>
        </div>
      </div>
    {/if}
  </div>

  <Tooltip
    content={app.videoUnderlayVisible
      ? 'Hide video underlay'
      : 'Show video underlay'}
    side="bottom"
    delay={TOOLTIP_DELAY}
  >
    <button
      onclick={() => app.setVideoUnderlayVisible(!app.videoUnderlayVisible)}
      class="hdr-btn hdr-btn-icon hdr-btn-ghost shrink-0 cursor-pointer {app.videoUnderlayVisible
        ? ''
        : 'text-[var(--dim)]'}"
      aria-label={app.videoUnderlayVisible
        ? 'Hide video underlay'
        : 'Show video underlay'}
      aria-pressed={app.videoUnderlayVisible}
    >
      {#if app.videoUnderlayVisible}
        <Eye size={12} />
      {:else}
        <EyeOff size={12} />
      {/if}
    </button>
  </Tooltip>
{/if}
