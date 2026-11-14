//! Motion-based video ↔ activity alignment.
//!
//! Cameras without GPS (e.g. GoPro HERO12) have only their own clock to place
//! footage on the ride, and that clock is often minutes off — or carries a
//! wrong timezone. GoPro footage does carry a GPMF telemetry track with the
//! accelerometer (`ACCL`) though, and road vibration on the handlebar tracks
//! riding speed closely: ~0 when stopped, rising with speed. Cross-correlating
//! a per-bin vibration signal from every clip against the activity's speed
//! finds the single clock offset that best explains all clips at once.
//!
//! All clips from one camera share the same clock error, so their *relative*
//! placement (from `creation_time`) is kept and only one global shift is
//! searched. That's what makes even 3-second clips useful: they ride along
//! with the long ones instead of needing enough signal on their own.

use serde::Serialize;
use std::process::Stdio;

use crate::render::activity::Activity;
use crate::render::scene::{ffmpeg_command, resolve_ffmpeg};

/// Width of one correlation bin. Vibration is noisy second to second (bumps,
/// out-of-saddle efforts); 5 s smooths that while keeping the stop/start
/// edges that pin the alignment down.
pub const BIN_SECS: f64 = 5.0;

/// A gap between consecutive activity samples longer than this means the
/// recorder auto-paused — the rider was (almost always) standing still.
const PAUSE_GAP_SECS: f64 = 5.0;

/// The correlation peak is a few bins wide (5 s bins over slowly varying
/// speed), so the runner-up — used to judge ambiguity — is taken from offsets
/// at least this far from the winner.
const RUNNER_UP_EXCLUSION_SECS: i64 = 180;

// ─── GPMF parsing ────────────────────────────────────────────────────────────

/// Accelerometer magnitude statistics for one GPMF payload (≈1 s).
fn magnitude_std(samples: &[[f64; 3]]) -> Option<f64> {
    if samples.len() < 2 {
        return None;
    }
    let mags: Vec<f64> = samples
        .iter()
        .map(|[x, y, z]| (x * x + y * y + z * z).sqrt())
        .collect();
    let mean = mags.iter().sum::<f64>() / mags.len() as f64;
    let var = mags.iter().map(|m| (m - mean).powi(2)).sum::<f64>() / mags.len() as f64;
    Some(var.sqrt())
}

/// Read `n` numeric values of GPMF type `t` (big-endian) from `d`.
fn read_numbers(t: u8, d: &[u8]) -> Vec<f64> {
    match t {
        b's' => d
            .chunks_exact(2)
            .map(|c| i16::from_be_bytes([c[0], c[1]]) as f64)
            .collect(),
        b'S' => d
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]) as f64)
            .collect(),
        b'l' => d
            .chunks_exact(4)
            .map(|c| i32::from_be_bytes([c[0], c[1], c[2], c[3]]) as f64)
            .collect(),
        b'L' => d
            .chunks_exact(4)
            .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]) as f64)
            .collect(),
        b'f' => d
            .chunks_exact(4)
            .map(|c| f32::from_be_bytes([c[0], c[1], c[2], c[3]]) as f64)
            .collect(),
        b'b' => d.iter().map(|&b| b as i8 as f64).collect(),
        b'B' => d.iter().map(|&b| b as f64).collect(),
        _ => Vec::new(),
    }
}

/// One KLV entry: (key, type, struct size, repeat, payload).
struct Klv<'a> {
    key: [u8; 4],
    kind: u8,
    size: usize,
    data: &'a [u8],
}

/// Iterate the KLV entries at one nesting level. Stops quietly at the first
/// malformed header — a truncated tail shouldn't throw away what parsed.
fn klvs(buf: &[u8]) -> impl Iterator<Item = Klv<'_>> {
    let mut i = 0usize;
    std::iter::from_fn(move || {
        if i + 8 > buf.len() {
            return None;
        }
        let key = [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]];
        let kind = buf[i + 4];
        let size = buf[i + 5] as usize;
        let repeat = u16::from_be_bytes([buf[i + 6], buf[i + 7]]) as usize;
        let len = size * repeat;
        let start = i + 8;
        let end = start + len;
        if end > buf.len() {
            return None;
        }
        i = start + len.div_ceil(4) * 4;
        Some(Klv {
            key,
            kind,
            size,
            data: &buf[start..end],
        })
    })
}

/// Collect scaled ACCL samples from one STRM (or any nested container).
/// `SCAL` applies to the sensor data that follows it inside the same stream:
/// a single divisor, or one per axis.
fn collect_accl(buf: &[u8], out: &mut Vec<[f64; 3]>) {
    let mut scal: Vec<f64> = vec![1.0];
    for k in klvs(buf) {
        if k.kind == 0 {
            collect_accl(k.data, out);
            continue;
        }
        match &k.key {
            b"SCAL" => {
                let v = read_numbers(k.kind, k.data);
                if !v.is_empty() && v.iter().all(|s| *s != 0.0) {
                    scal = v;
                }
            }
            b"ACCL" => {
                let vals = read_numbers(k.kind, k.data);
                let per_sample = match k.kind {
                    b's' | b'S' => k.size / 2,
                    b'l' | b'L' | b'f' => k.size / 4,
                    b'b' | b'B' => k.size,
                    _ => 0,
                };
                if per_sample < 3 {
                    continue;
                }
                for s in vals.chunks_exact(per_sample) {
                    let sc = |axis: usize| *scal.get(axis).unwrap_or(&scal[0]);
                    out.push([s[0] / sc(0), s[1] / sc(1), s[2] / sc(2)]);
                }
            }
            _ => {}
        }
    }
}

/// Vibration value per top-level `DEVC` block in a raw GPMF stream (as
/// written by `ffmpeg -c copy -f data`). Each DEVC is one telemetry payload,
/// normally ~1 s of sensor data. Blocks without accelerometer data yield
/// `None` so the time grid stays aligned with the payload count.
pub fn vibration_per_payload(gpmf: &[u8]) -> Vec<Option<f64>> {
    klvs(gpmf)
        .filter(|k| &k.key == b"DEVC" && k.kind == 0)
        .map(|devc| {
            let mut samples = Vec::new();
            collect_accl(devc.data, &mut samples);
            magnitude_std(&samples)
        })
        .collect()
}

/// Dump a clip's GPMF track with the bundled ffmpeg and turn it into
/// `(seconds from clip start, vibration)` samples. Payloads are spread evenly
/// across the clip duration — GoPro writes one per ~1.001 s, and the
/// correlation works on 5 s bins, so sub-second timing doesn't matter.
pub fn extract_vibration(
    path: &str,
    gpmf_stream: usize,
    duration: f64,
) -> Result<Vec<(f64, f64)>, String> {
    let ffmpeg_bin = resolve_ffmpeg();
    let mut cmd = ffmpeg_command(&ffmpeg_bin);
    cmd.args([
        "-v",
        "error",
        "-i",
        path,
        "-map",
        &format!("0:{gpmf_stream}"),
        "-c",
        "copy",
        "-f",
        "data",
        "-",
    ]);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let out = cmd
        .output()
        .map_err(|e| format!("Failed to spawn ffmpeg ({ffmpeg_bin}): {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ffmpeg could not read the telemetry track of {path}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let per_payload = vibration_per_payload(&out.stdout);
    let n = per_payload.len();
    if n == 0 || duration <= 0.0 {
        return Ok(Vec::new());
    }
    let step = duration / n as f64;
    Ok(per_payload
        .into_iter()
        .enumerate()
        .filter_map(|(i, v)| v.map(|v| ((i as f64 + 0.5) * step, v)))
        .collect())
}

/// Average `(t, value)` samples into fixed-width bins covering `duration`.
pub fn bin_samples(samples: &[(f64, f64)], duration: f64) -> Vec<Option<f64>> {
    let nbins = (duration / BIN_SECS).ceil().max(0.0) as usize;
    let mut sum = vec![0.0; nbins];
    let mut cnt = vec![0usize; nbins];
    for &(t, v) in samples {
        let b = (t / BIN_SECS).floor();
        if b >= 0.0 && (b as usize) < nbins && v.is_finite() {
            sum[b as usize] += v;
            cnt[b as usize] += 1;
        }
    }
    sum.into_iter()
        .zip(cnt)
        .map(|(s, c)| (c > 0).then(|| s / c as f64))
        .collect()
}

// ─── Activity speed ──────────────────────────────────────────────────────────

/// Speed (activity units) for every whole second of the activity, `None`
/// where unknown. Auto-pause gaps count as standing still (speed 0), which is
/// exactly the signal that lines up with a near-silent accelerometer.
pub fn speed_per_second(activity: &Activity) -> Vec<Option<f64>> {
    let t = &activity.elapsed_seconds;
    let v = &activity.speed;
    if t.len() < 2 || t.len() != v.len() {
        return Vec::new();
    }
    let total = t[t.len() - 1].ceil().max(0.0) as usize + 1;
    let mut out = vec![None; total];
    for i in 0..t.len() - 1 {
        let (a, b) = (t[i], t[i + 1]);
        let dt = b - a;
        if dt.is_nan() || dt <= 0.0 {
            continue;
        }
        let val = if dt > PAUSE_GAP_SECS { 0.0 } else { v[i] };
        if !val.is_finite() {
            continue;
        }
        let (from, to) = (a.floor().max(0.0) as usize, (b.floor() as usize).min(total));
        for slot in out.iter_mut().take(to).skip(from) {
            *slot = Some(val);
        }
    }
    if let Some(last) = v.last().copied().filter(|x| x.is_finite()) {
        let k = t[t.len() - 1].floor() as usize;
        if k < total {
            out[k] = Some(last);
        }
    }
    out
}

/// Prefix sums over a per-second series so any window mean is O(1).
struct WindowMeans {
    sum: Vec<f64>,
    cnt: Vec<u32>,
}

impl WindowMeans {
    fn new(series: &[Option<f64>]) -> Self {
        let mut sum = Vec::with_capacity(series.len() + 1);
        let mut cnt = Vec::with_capacity(series.len() + 1);
        sum.push(0.0);
        cnt.push(0);
        for v in series {
            sum.push(sum.last().unwrap() + v.unwrap_or(0.0));
            cnt.push(cnt.last().unwrap() + v.is_some() as u32);
        }
        WindowMeans { sum, cnt }
    }

    /// Mean over seconds `[start, start + len)`, or `None` when fewer than
    /// half the seconds are known (outside the activity, or a data hole).
    fn mean(&self, start: i64, len: usize) -> Option<f64> {
        let n = self.sum.len() as i64 - 1;
        let a = start.max(0);
        let b = (start + len as i64).min(n);
        if b <= a {
            return None;
        }
        let c = self.cnt[b as usize] - self.cnt[a as usize];
        if (c as usize) * 2 < len {
            return None;
        }
        Some((self.sum[b as usize] - self.sum[a as usize]) / c as f64)
    }
}

// ─── Correlation search ──────────────────────────────────────────────────────

/// One clip's motion signal positioned on the activity time axis.
pub struct ClipMotion {
    /// Where the clip's first frame currently sits, in seconds from the
    /// activity start (camera clock based — the thing being corrected).
    pub start_sec: f64,
    /// Vibration per [`BIN_SECS`] bin from the clip start.
    pub bins: Vec<Option<f64>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OffsetMatch {
    /// Seconds to add to every clip's current start to align it.
    pub offset_sec: f64,
    /// Pearson correlation between vibration and speed at that offset.
    pub correlation: f64,
    /// Best correlation found more than 3 minutes away from `offset_sec` —
    /// a runner-up close to the winner means the match is ambiguous.
    pub runner_up: f64,
    /// Number of 5 s bins that overlapped the activity at the chosen offset.
    pub bins_used: usize,
    /// "high" | "medium" | "low".
    pub confidence: &'static str,
}

fn pearson(xs: &[f64], ys: &[f64]) -> Option<f64> {
    let n = xs.len() as f64;
    if xs.len() < 3 {
        return None;
    }
    let mx = xs.iter().sum::<f64>() / n;
    let my = ys.iter().sum::<f64>() / n;
    let (mut c, mut vx, mut vy) = (0.0, 0.0, 0.0);
    for (x, y) in xs.iter().zip(ys) {
        c += (x - mx) * (y - my);
        vx += (x - mx).powi(2);
        vy += (y - my).powi(2);
    }
    let d = (vx * vy).sqrt();
    (d > 0.0).then(|| c / d)
}

/// Correlation at one candidate offset; `None` when too few bins overlap.
fn score(
    clips: &[ClipMotion],
    speed: &WindowMeans,
    offset: i64,
    min_bins: usize,
) -> Option<(f64, usize)> {
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for clip in clips {
        let base = clip.start_sec.floor() as i64 + offset;
        for (i, v) in clip.bins.iter().enumerate() {
            let Some(v) = v else { continue };
            let t = base + (i as f64 * BIN_SECS) as i64;
            if let Some(s) = speed.mean(t, BIN_SECS as usize) {
                xs.push(*v);
                ys.push(s);
            }
        }
    }
    if xs.len() < min_bins {
        return None;
    }
    pearson(&xs, &ys).map(|r| (r, xs.len()))
}

/// Search every offset at which the clips overlap the activity for the one
/// where clip vibration best tracks activity speed.
///
/// Coarse pass in whole-bin steps across the full overlap range, then a
/// 1-second refinement around the winner. Offsets where only a sliver of the
/// footage overlaps the ride are rejected (`min_bins`), since a handful of
/// bins can correlate highly by chance.
pub fn find_offset(
    clips: &[ClipMotion],
    speed_per_sec: &[Option<f64>],
) -> Result<OffsetMatch, String> {
    let total_bins: usize = clips
        .iter()
        .map(|c| c.bins.iter().filter(|b| b.is_some()).count())
        .sum();
    if total_bins < 12 {
        return Err(
            "Not enough motion data to auto-sync — the clips need at least a minute of footage with accelerometer data in total."
                .to_string(),
        );
    }
    if speed_per_sec.is_empty() {
        return Err("The activity has no speed data to sync against.".to_string());
    }
    let min_bins = ((total_bins as f64 * 0.6).ceil() as usize).max(12);
    let speed = WindowMeans::new(speed_per_sec);
    let activity_len = speed_per_sec.len() as f64;

    let earliest = clips
        .iter()
        .map(|c| c.start_sec)
        .fold(f64::INFINITY, f64::min);
    let latest_end = clips
        .iter()
        .map(|c| c.start_sec + c.bins.len() as f64 * BIN_SECS)
        .fold(f64::NEG_INFINITY, f64::max);
    let lo = (-latest_end).floor() as i64;
    let hi = (activity_len - earliest).ceil() as i64;
    let step = BIN_SECS as i64;

    let mut coarse: Vec<(i64, f64, usize)> = Vec::new();
    let mut off = lo;
    while off <= hi {
        if let Some((r, n)) = score(clips, &speed, off, min_bins) {
            coarse.push((off, r, n));
        }
        off += step;
    }
    // Footage is normally shot *during* the ride, so the true offset puts
    // (nearly) every bin inside the activity. Offsets that push part of the
    // footage off either end lose bins — and a smaller sample can correlate
    // higher by chance — so only offsets within 10% of the best achievable
    // overlap compete.
    let max_overlap = coarse.iter().map(|c| c.2).max().unwrap_or(0);
    let min_overlap = ((max_overlap as f64 * 0.9).ceil() as usize).max(min_bins);
    coarse.retain(|c| c.2 >= min_overlap);
    let Some(&(best_coarse, _, _)) = coarse.iter().max_by(|a, b| a.1.total_cmp(&b.1)) else {
        return Err(
            "The videos don't overlap the activity at any offset — check that the right activity is loaded."
                .to_string(),
        );
    };

    let mut best = (best_coarse, f64::NEG_INFINITY, 0usize);
    for off in (best_coarse - step * 2)..=(best_coarse + step * 2) {
        if let Some((r, n)) = score(clips, &speed, off, min_overlap)
            && r > best.1
        {
            best = (off, r, n);
        }
    }
    let runner_up = coarse
        .iter()
        .filter(|(o, _, _)| (o - best.0).abs() > RUNNER_UP_EXCLUSION_SECS)
        .map(|(_, r, _)| *r)
        .fold(f64::NEG_INFINITY, f64::max);
    let runner_up = if runner_up.is_finite() {
        runner_up
    } else {
        0.0
    };

    let margin = best.1 - runner_up;
    let confidence = if best.1 >= 0.3 && margin >= 0.08 {
        "high"
    } else if best.1 >= 0.2 && margin >= 0.03 {
        "medium"
    } else {
        "low"
    };

    Ok(OffsetMatch {
        offset_sec: best.0 as f64,
        correlation: best.1,
        runner_up,
        bins_used: best.2,
        confidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn klv(key: &[u8; 4], kind: u8, size: u8, repeat: u16, data: &[u8]) -> Vec<u8> {
        let mut v = key.to_vec();
        v.push(kind);
        v.push(size);
        v.extend_from_slice(&repeat.to_be_bytes());
        v.extend_from_slice(data);
        while !v.len().is_multiple_of(4) {
            v.push(0);
        }
        v
    }

    fn accl_devc(samples: &[[i16; 3]], scal: i16) -> Vec<u8> {
        let mut data = Vec::new();
        for s in samples {
            for a in s {
                data.extend_from_slice(&a.to_be_bytes());
            }
        }
        let mut strm = klv(b"STNM", b'c', 5, 1, b"Accel");
        strm.extend(klv(b"SCAL", b's', 2, 1, &scal.to_be_bytes()));
        strm.extend(klv(b"ACCL", b's', 6, samples.len() as u16, &data));
        let strm = klv(b"STRM", 0, 1, strm.len() as u16, &strm);
        let mut devc = klv(b"DVNM", b'c', 6, 1, b"Camera");
        devc.extend(strm);
        klv(b"DEVC", 0, 1, devc.len() as u16, &devc)
    }

    #[test]
    fn parses_scaled_accelerometer_per_devc() {
        // Still camera: constant 1 g magnitude → zero vibration.
        let still = accl_devc(&[[0, 0, 418], [0, 0, 418], [0, 0, 418]], 418);
        // Shaking: magnitudes 1 g and 2 g alternate → std 0.5 g.
        let shake = accl_devc(&[[0, 0, 418], [0, 0, 836], [0, 0, 418], [0, 0, 836]], 418);
        let mut stream = still;
        stream.extend(shake);
        let v = vibration_per_payload(&stream);
        assert_eq!(v.len(), 2);
        assert!(v[0].unwrap().abs() < 1e-9);
        assert!((v[1].unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn truncated_stream_keeps_complete_payloads() {
        let mut stream = accl_devc(&[[0, 0, 1], [0, 0, 2]], 1);
        let full = stream.len();
        stream.extend(accl_devc(&[[0, 0, 1], [0, 0, 3]], 1));
        stream.truncate(full + 10);
        assert_eq!(vibration_per_payload(&stream).len(), 1);
    }

    #[test]
    fn bins_average_samples() {
        let s = [(0.5, 1.0), (1.5, 3.0), (6.0, 10.0)];
        assert_eq!(bin_samples(&s, 11.0), vec![Some(2.0), Some(10.0), None]);
    }

    #[test]
    fn pause_gap_counts_as_stopped() {
        let a = Activity {
            elapsed_seconds: vec![0.0, 1.0, 2.0, 20.0, 21.0],
            speed: vec![5.0, 5.0, 5.0, 6.0, 6.0],
            ..Default::default()
        };
        let s = speed_per_second(&a);
        assert_eq!(s[1], Some(5.0));
        assert_eq!(s[10], Some(0.0));
        assert_eq!(s[20], Some(6.0));
        assert_eq!(s.len(), 22);
    }

    /// Synthetic ride: alternating riding/stopped stretches. Clips recorded
    /// with a camera clock 1234 s fast must be pulled back by -1234 s.
    #[test]
    fn recovers_camera_clock_error() {
        let n = 4 * 3600;
        let speed: Vec<Option<f64>> = (0..n)
            .map(|t| {
                // Irregular stop pattern so no other offset fits as well.
                let phase = (t / 97 + t / 413 + t / 1201) % 3;
                Some(if phase == 0 {
                    0.0
                } else {
                    8.0 + (t % 17) as f64 * 0.1
                })
            })
            .collect();
        let clock_error = 1234.0;
        let make_clip = |true_start: usize, len: usize| {
            let samples: Vec<(f64, f64)> = (0..len)
                .map(|i| {
                    let v = speed[true_start + i].unwrap();
                    (
                        i as f64 + 0.5,
                        0.2 + v * 0.3 + ((i * 7919) % 13) as f64 * 0.05,
                    )
                })
                .collect();
            ClipMotion {
                start_sec: true_start as f64 + clock_error,
                bins: bin_samples(&samples, len as f64),
            }
        };
        let clips = vec![
            make_clip(600, 300),
            make_clip(2000, 900),
            make_clip(5000, 20),
            make_clip(9000, 400),
        ];
        let m = find_offset(&clips, &speed).unwrap();
        assert!((m.offset_sec + clock_error).abs() <= 2.0, "{m:?}");
        assert!(m.correlation > 0.8, "{m:?}");
        assert_eq!(m.confidence, "high");
    }

    #[test]
    fn rejects_too_little_footage() {
        let clips = vec![ClipMotion {
            start_sec: 0.0,
            bins: vec![Some(1.0); 3],
        }];
        assert!(find_offset(&clips, &[Some(1.0); 100]).is_err());
    }
}
