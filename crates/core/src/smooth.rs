//! Smoothing of per-frame series.
//!
//! [`robust_local_linear`] is the workhorse: a Gaussian-weighted local linear fit at every frame (so a
//! steady brightening at the start or end of a sequence is followed, not flattened as a plain moving
//! average would), repeated with bisquare weights so single outliers (headlights, a flash, a bird in
//! front of the lens) do not pull the curve.

/// Gaussian-weighted local linear regression of `values` at every index.
///
/// `sigma` is the Gaussian width in frames; the kernel is cut at 3 sigma. `weights` (same length as
/// `values`, each ≥ 0) scale each sample's influence; `None` means equal weights. Non-finite values get
/// weight zero. An index whose neighbourhood has no usable sample returns the input value.
#[must_use]
pub fn local_linear(values: &[f64], weights: Option<&[f64]>, sigma: f64) -> Vec<f64> {
    let n = values.len();
    if n == 0 {
        return Vec::new();
    }
    if !(sigma.is_finite() && sigma > 0.0) {
        return values.to_vec();
    }
    // Truncation: ±3 sigma covers 99.7 % of the kernel; at least one neighbour each side.
    let radius = ((3.0 * sigma).ceil() as usize).max(1);
    let inv_two_sigma2 = 1.0 / (2.0 * sigma * sigma);
    let kernel: Vec<f64> = (0..=radius)
        .map(|d| (-((d * d) as f64) * inv_two_sigma2).exp())
        .collect();

    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let lo = i.saturating_sub(radius);
        let hi = (i + radius).min(n - 1);
        // Weighted least squares of y = a + b·(x − i), evaluated at x = i (so the result is a).
        let (mut s0, mut s1, mut s2, mut t0, mut t1) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (j, &y) in values.iter().enumerate().take(hi + 1).skip(lo) {
            if !y.is_finite() {
                continue;
            }
            let extra = weights.and_then(|w| w.get(j)).copied().unwrap_or(1.0);
            if !(extra.is_finite() && extra > 0.0) {
                continue;
            }
            let k = kernel.get(i.abs_diff(j)).copied().unwrap_or(0.0) * extra;
            let x = j as f64 - i as f64;
            s0 += k;
            s1 += k * x;
            s2 += k * x * x;
            t0 += k * y;
            t1 += k * x * y;
        }
        let own = values.get(i).copied().unwrap_or(f64::NAN);
        let det = s0 * s2 - s1 * s1;
        let fitted = if s0 <= 0.0 {
            own
        } else if det.abs() <= 1e-12 * s0 * s0.max(1.0) {
            // One usable sample (or all at the same x): fall back to the weighted mean.
            t0 / s0
        } else {
            (s2 * t0 - s1 * t1) / det
        };
        out.push(fitted);
    }
    out
}

/// Gaussian-weighted local quadratic regression of `values` at every index: as [`local_linear`],
/// but a bend (the knee where a ramp ends, a moonrise) is followed instead of rounded off.
#[must_use]
pub fn local_quadratic(values: &[f64], weights: Option<&[f64]>, sigma: f64) -> Vec<f64> {
    let n = values.len();
    if n == 0 {
        return Vec::new();
    }
    if !(sigma.is_finite() && sigma > 0.0) {
        return values.to_vec();
    }
    let radius = ((3.0 * sigma).ceil() as usize).max(2);
    let inv_two_sigma2 = 1.0 / (2.0 * sigma * sigma);
    let kernel: Vec<f64> = (0..=radius)
        .map(|d| (-((d * d) as f64) * inv_two_sigma2).exp())
        .collect();
    let linear = local_linear(values, weights, sigma);
    let det3 = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    (0..n)
        .map(|i| {
            let lo = i.saturating_sub(radius);
            let hi = (i + radius).min(n - 1);
            let (mut s, mut t) = ([0.0f64; 5], [0.0f64; 3]);
            for (j, &y) in values.iter().enumerate().take(hi + 1).skip(lo) {
                if !y.is_finite() {
                    continue;
                }
                let extra = weights.and_then(|w| w.get(j)).copied().unwrap_or(1.0);
                if !(extra.is_finite() && extra > 0.0) {
                    continue;
                }
                let k = kernel.get(i.abs_diff(j)).copied().unwrap_or(0.0) * extra;
                let x = j as f64 - i as f64;
                let mut p = k;
                for (e, slot) in s.iter_mut().enumerate() {
                    *slot += p;
                    if let Some(tt) = t.get_mut(e) {
                        *tt += p * y;
                    }
                    p *= x;
                }
            }
            let m = [[s[0], s[1], s[2]], [s[1], s[2], s[3]], [s[2], s[3], s[4]]];
            let d = det3(m);
            // Too few samples for a bend (the ends, gaps): the local line.
            if s[0] <= 0.0 || d.abs() <= 1e-9 * s[0].powi(3).max(1.0) {
                return linear.get(i).copied().unwrap_or(f64::NAN);
            }
            let mut ma = m;
            for (row, y) in ma.iter_mut().zip(t) {
                row[0] = y;
            }
            let a = det3(ma) / d;
            let line = linear.get(i).copied().unwrap_or(f64::NAN);
            if !a.is_finite() {
                return line;
            }
            // Towards the ends a parabola fitted to one side only swings: blended into the line
            // there, smoothly (no step in the corrections where it changes).
            // Only the outermost frames (a quarter of the kernel's reach): further in, the line
            // would round off a knee near the start again (TNT's 0.0050 became 0.0240).
            let inside = (i.min(n - 1 - i) as f64 / (radius as f64 / 4.0).max(1.0)).clamp(0.0, 1.0);
            line + inside * (a - line)
        })
        .collect()
}

/// [`local_quadratic`] with bisquare re-weighting, as [`robust_local_linear`].
#[must_use]
pub fn robust_local_quadratic(values: &[f64], sigma: f64, iterations: u32) -> Vec<f64> {
    let mut fit = local_quadratic(values, None, sigma);
    for _ in 0..iterations {
        let residuals: Vec<f64> = values.iter().zip(&fit).map(|(y, f)| (y - f).abs()).collect();
        let Some(mad) = median(&residuals) else { break };
        let largest = residuals
            .iter()
            .copied()
            .filter(|r| r.is_finite())
            .fold(0.0, f64::max);
        if largest <= 1e-12 {
            break;
        }
        let scale = 6.0 * mad.max(1e-3 * largest);
        let weights: Vec<f64> = residuals
            .iter()
            .map(|r| {
                let u = r / scale;
                if u.is_finite() && u < 1.0 {
                    (1.0 - u * u).powi(2)
                } else {
                    0.0
                }
            })
            .collect();
        fit = local_quadratic(values, Some(&weights), sigma);
    }
    fit
}

/// [`local_linear`] with `iterations` rounds of bisquare re-weighting (LOWESS style).
///
/// After each fit, samples whose residual is large compared with the median absolute residual lose
/// weight; beyond 6 median deviations they are ignored. `iterations = 0` is a plain [`local_linear`].
#[must_use]
pub fn robust_local_linear(values: &[f64], sigma: f64, iterations: u32) -> Vec<f64> {
    let mut fit = local_linear(values, None, sigma);
    for _ in 0..iterations {
        let residuals: Vec<f64> = values.iter().zip(&fit).map(|(y, f)| (y - f).abs()).collect();
        let Some(mad) = median(&residuals) else { break };
        let largest = residuals
            .iter()
            .copied()
            .filter(|r| r.is_finite())
            .fold(0.0, f64::max);
        // A perfect fit needs no re-weighting.
        if largest <= 1e-12 {
            break;
        }
        // More than half the residuals can be exactly zero (a still scene with one flash), which makes
        // the MAD zero; a floor relative to the largest residual still rejects the outlier.
        let scale = 6.0 * mad.max(1e-3 * largest);
        let weights: Vec<f64> = residuals
            .iter()
            .map(|r| {
                let u = r / scale;
                if u.is_finite() && u < 1.0 {
                    (1.0 - u * u).powi(2)
                } else {
                    0.0
                }
            })
            .collect();
        fit = local_linear(values, Some(&weights), sigma);
    }
    fit
}

/// Median of the finite values, `None` when there are none.
#[must_use]
pub fn median(values: &[f64]) -> Option<f64> {
    let mut finite: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.is_empty() {
        return None;
    }
    finite.sort_by(f64::total_cmp);
    let mid = finite.len() / 2;
    let upper = *finite.get(mid)?;
    if finite.len() % 2 == 1 {
        Some(upper)
    } else {
        Some(0.5 * (finite.get(mid - 1)? + upper))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn a_knee_is_followed_more_closely_by_the_parabola() {
        // A ramp down that ends at frame 100 (a sunset's end), then level.
        let v: Vec<f64> = (0..200)
            .map(|i| if i < 100 { -0.05 * (100 - i) as f64 } else { 0.0 })
            .collect();
        let line = local_linear(&v, None, 8.0);
        let bend = local_quadratic(&v, None, 12.0);
        let off = |f: &[f64]| (80..120).map(|i| (f[i] - v[i]).abs()).fold(0.0, f64::max);
        assert!(off(&bend) < 0.8 * off(&line), "{} {}", off(&bend), off(&line));
        // A straight line stays exactly straight, ends included.
        let straight: Vec<f64> = (0..60).map(|i| 0.1 * i as f64).collect();
        for (a, b) in local_quadratic(&straight, None, 12.0).iter().zip(&straight) {
            assert!((a - b).abs() < 1e-9);
        }
    }

    #[test]
    fn linear_trend_is_kept_exactly_including_the_ends() {
        let values: Vec<f64> = (0..50).map(|i| 0.1 * i as f64 - 2.0).collect();
        let fit = local_linear(&values, None, 5.0);
        for (v, f) in values.iter().zip(&fit) {
            assert!(close(*v, *f, 1e-9), "{v} vs {f}");
        }
    }

    #[test]
    fn noise_is_reduced() {
        // Alternating ±0.2 around a constant: the fit should be near the constant.
        let values: Vec<f64> = (0..100).map(|i| if i % 2 == 0 { 1.2 } else { 0.8 }).collect();
        let fit = local_linear(&values, None, 4.0);
        for f in &fit[10..90] {
            assert!(close(*f, 1.0, 0.01), "{f}");
        }
    }

    #[test]
    fn robust_fit_ignores_a_single_outlier() {
        let mut values = vec![0.0; 60];
        values[30] = 3.0; // a flash
        let plain = local_linear(&values, None, 3.0);
        let robust = robust_local_linear(&values, 3.0, 3);
        assert!(plain[30] > 0.2, "plain fit should be pulled: {}", plain[30]);
        assert!(robust[30].abs() < 1e-6, "robust fit should not: {}", robust[30]);
        assert!(robust[29].abs() < 1e-6);
    }

    #[test]
    fn non_finite_samples_are_skipped() {
        let values = vec![1.0, 1.0, f64::NAN, 1.0, 1.0];
        let fit = local_linear(&values, None, 1.0);
        assert!(fit.iter().all(|f| close(*f, 1.0, 1e-9)), "{fit:?}");
    }

    #[test]
    fn degenerate_inputs() {
        assert!(local_linear(&[], None, 2.0).is_empty());
        assert_eq!(local_linear(&[4.0], None, 2.0), vec![4.0]);
        assert_eq!(local_linear(&[1.0, 2.0], None, 0.0), vec![1.0, 2.0]);
        assert_eq!(local_linear(&[1.0, 2.0], None, f64::NAN), vec![1.0, 2.0]);
        let all_nan = local_linear(&[f64::NAN, f64::NAN], None, 1.0);
        assert!(all_nan.iter().all(|v| v.is_nan()));
    }

    #[test]
    fn median_odd_even_and_empty() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(median(&[f64::NAN]), None);
        assert_eq!(median(&[]), None);
    }
}
