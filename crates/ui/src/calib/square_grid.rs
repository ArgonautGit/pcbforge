use image::GrayImage;
#[cfg(test)]
use image::Luma;
use nalgebra::Point2;
use vision::Homography;

use super::{DotPair, GridSpec};

#[derive(Debug, Clone, Copy)]
struct Candidate {
    px: Point2<f64>,
    delta: (f64, f64),
    score: f64,
}

struct Prepared {
    width: usize,
    height: usize,
    contrast: Vec<f64>,
    gx: Vec<f64>,
    gy: Vec<f64>,
}

impl Prepared {
    fn new(frame: &GrayImage, radius: usize) -> Self {
        let (width, height) = (frame.width() as usize, frame.height() as usize);
        let stride = width + 1;
        let mut sum = vec![0.0; stride * (height + 1)];
        let mut sum_sq = vec![0.0; stride * (height + 1)];
        for y in 0..height {
            let (mut row, mut row_sq) = (0.0, 0.0);
            for x in 0..width {
                let value = frame.get_pixel(x as u32, y as u32)[0] as f64;
                row += value;
                row_sq += value * value;
                sum[(y + 1) * stride + x + 1] = sum[y * stride + x + 1] + row;
                sum_sq[(y + 1) * stride + x + 1] = sum_sq[y * stride + x + 1] + row_sq;
            }
        }
        let area_sum = |table: &[f64], x0: usize, y0: usize, x1: usize, y1: usize| {
            table[y1 * stride + x1] - table[y0 * stride + x1] - table[y1 * stride + x0]
                + table[y0 * stride + x0]
        };
        let mut contrast = vec![0.0; width * height];
        for y in 0..height {
            let y0 = y.saturating_sub(radius);
            let y1 = (y + radius + 1).min(height);
            for x in 0..width {
                let x0 = x.saturating_sub(radius);
                let x1 = (x + radius + 1).min(width);
                let count = ((x1 - x0) * (y1 - y0)) as f64;
                let mean = area_sum(&sum, x0, y0, x1, y1) / count;
                let variance = (area_sum(&sum_sq, x0, y0, x1, y1) / count - mean * mean).max(0.0);
                // A small noise floor prevents quiet dark areas from being
                // amplified into convincing texture.
                let sigma = variance.sqrt().max(4.0);
                let value = frame.get_pixel(x as u32, y as u32)[0] as f64;
                contrast[y * width + x] = ((value - mean) / sigma).clamp(-6.0, 6.0);
            }
        }

        let mut gx = vec![0.0; width * height];
        let mut gy = vec![0.0; width * height];
        if width >= 3 && height >= 3 {
            for y in 1..height - 1 {
                for x in 1..width - 1 {
                    let at = |dx: isize, dy: isize| {
                        contrast[(y.wrapping_add_signed(dy)) * width + x.wrapping_add_signed(dx)]
                    };
                    gx[y * width + x] =
                        (-at(-1, -1) + at(1, -1) - 2.0 * at(-1, 0) + 2.0 * at(1, 0) - at(-1, 1)
                            + at(1, 1))
                            / 8.0;
                    gy[y * width + x] = (-at(-1, -1) - 2.0 * at(0, -1) - at(1, -1)
                        + at(-1, 1)
                        + 2.0 * at(0, 1)
                        + at(1, 1))
                        / 8.0;
                }
            }
        }
        Self {
            width,
            height,
            contrast,
            gx,
            gy,
        }
    }

    fn bilinear(&self, values: &[f64], p: Point2<f64>) -> f64 {
        if p.x < 0.0
            || p.y < 0.0
            || p.x >= (self.width.saturating_sub(1)) as f64
            || p.y >= (self.height.saturating_sub(1)) as f64
        {
            return 0.0;
        }
        let (x0, y0) = (p.x.floor() as usize, p.y.floor() as usize);
        let (tx, ty) = (p.x - x0 as f64, p.y - y0 as f64);
        let at = |x: usize, y: usize| values[y * self.width + x];
        let top = at(x0, y0) * (1.0 - tx) + at(x0 + 1, y0) * tx;
        let bottom = at(x0, y0 + 1) * (1.0 - tx) + at(x0 + 1, y0 + 1) * tx;
        top * (1.0 - ty) + bottom * ty
    }

    fn gradient(&self, p: Point2<f64>) -> (f64, f64) {
        (self.bilinear(&self.gx, p), self.bilinear(&self.gy, p))
    }

    fn value(&self, p: Point2<f64>) -> f64 {
        self.bilinear(&self.contrast, p)
    }

    #[cfg(test)]
    fn preview(&self) -> GrayImage {
        GrayImage::from_fn(self.width as u32, self.height as u32, |x, y| {
            let value = self.contrast[y as usize * self.width + x as usize];
            Luma([((value / 6.0 * 127.0) + 128.0).clamp(0.0, 255.0) as u8])
        })
    }
}

fn unit(vector: nalgebra::Vector2<f64>) -> Option<nalgebra::Vector2<f64>> {
    let norm = vector.norm();
    (norm > 1e-9 && norm.is_finite()).then_some(vector / norm)
}

fn square_score(
    image: &Prepared,
    center: Point2<f64>,
    axis_x: nalgebra::Vector2<f64>,
    axis_y: nalgebra::Vector2<f64>,
    half_x: f64,
    half_y: f64,
) -> f64 {
    let Some(dir_x) = unit(axis_x) else {
        return 0.0;
    };
    let Some(dir_y) = unit(axis_y) else {
        return 0.0;
    };
    let normal_x = nalgebra::Vector2::new(dir_y.y, -dir_y.x);
    let normal_y = nalgebra::Vector2::new(-dir_x.y, dir_x.x);
    let samples = ((half_x.max(half_y) * 2.0).ceil() as usize).clamp(5, 19);
    let edge = |offset: nalgebra::Vector2<f64>,
                along: nalgebra::Vector2<f64>,
                half: f64,
                normal: nalgebra::Vector2<f64>| {
        let mut total = 0.0;
        for i in 0..samples {
            let t = if samples == 1 {
                0.0
            } else {
                -half + 2.0 * half * i as f64 / (samples - 1) as f64
            };
            let p = center + offset + along * t;
            let (gx, gy) = image.gradient(p);
            total += (gx * normal.x + gy * normal.y).abs();
        }
        total / samples as f64
    };
    let left = edge(-dir_x * half_x, dir_y, half_y, normal_x);
    let right = edge(dir_x * half_x, dir_y, half_y, normal_x);
    let top = edge(-dir_y * half_y, dir_x, half_x, normal_y);
    let bottom = edge(dir_y * half_y, dir_x, half_x, normal_y);
    let harmonic = |a: f64, b: f64| 2.0 * a * b / (a + b + 1e-9);
    let vertical = harmonic(left, right);
    let horizontal = harmonic(top, bottom);
    let edge_balance = 2.0 * vertical.min(horizontal) / (vertical + horizontal + 1e-9);

    let core_offsets = [
        (0.0, 0.0),
        (-0.35, -0.35),
        (0.35, -0.35),
        (-0.35, 0.35),
        (0.35, 0.35),
    ];
    let core = core_offsets
        .iter()
        .map(|&(x, y)| image.value(center + dir_x * (x * half_x) + dir_y * (y * half_y)))
        .sum::<f64>()
        / core_offsets.len() as f64;
    let ring_offsets = [
        (-1.55, 0.0),
        (1.55, 0.0),
        (0.0, -1.55),
        (0.0, 1.55),
        (-1.1, -1.1),
        (1.1, -1.1),
        (-1.1, 1.1),
        (1.1, 1.1),
    ];
    let ring = ring_offsets
        .iter()
        .map(|&(x, y)| image.value(center + dir_x * (x * half_x) + dir_y * (y * half_y)))
        .sum::<f64>()
        / ring_offsets.len() as f64;
    let contrast = (core - ring).abs();
    (vertical * horizontal).sqrt() * (0.35 + 0.65 * edge_balance) + 0.35 * contrast
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut values = values.to_vec();
    let middle = values.len() / 2;
    let (_, value, _) = values.select_nth_unstable_by(middle, f64::total_cmp);
    *value
}

fn site_candidates(
    image: &Prepared,
    seed: &Homography,
    mm: Point2<f64>,
    grid: &GridSpec,
    dot_mm: f64,
    search_px: f64,
) -> Vec<Candidate> {
    let expected = seed.apply(mm);
    let dx = seed.apply(Point2::new(mm.x + 1.0, mm.y)) - expected;
    let dy = seed.apply(Point2::new(mm.x, mm.y + 1.0)) - expected;
    let (sx, sy) = (dx.norm(), dy.norm());
    if sx <= 1e-9 || sy <= 1e-9 {
        return Vec::new();
    }
    let nominal = dot_mm * 0.5 * (sx + sy);
    let max_side = (grid.pitch_mm * 0.22 * sx.min(sy)).max(3.0);
    let min_side = nominal.max(2.5).min(max_side);
    let scales = [1.0, 1.45, 2.0, 2.8, 3.7];
    let sides: Vec<f64> = scales
        .into_iter()
        .map(|scale| (nominal * scale).clamp(min_side, max_side))
        .fold(Vec::new(), |mut values, side| {
            if values.last().is_none_or(|last| (last - side).abs() > 0.4) {
                values.push(side);
            }
            values
        });
    let radius = search_px.ceil() as i32;
    let response_at = |x: i32, y: i32| {
        let delta = (x as f64 - expected.x, y as f64 - expected.y);
        if delta.0.hypot(delta.1) > search_px {
            return None;
        }
        let center = Point2::new(x as f64, y as f64);
        let score = sides
            .iter()
            .map(|&side| {
                square_score(
                    image,
                    center,
                    dx,
                    dy,
                    side * sx / (sx + sy),
                    side * sy / (sx + sy),
                )
            })
            .fold(0.0_f64, f64::max);
        Some((center, score))
    };
    // Search coarsely, then evaluate full-resolution neighborhoods around the
    // strongest separated peaks. This keeps live re-anchoring responsive on
    // high-resolution frames without sacrificing sub-pixel centroiding.
    let step = ((search_px / 14.0).ceil() as usize).clamp(1, 3);
    let x0 = expected.x.round() as i32 - radius;
    let x1 = expected.x.round() as i32 + radius;
    let y0 = expected.y.round() as i32 - radius;
    let y1 = expected.y.round() as i32 + radius;
    let mut responses = Vec::new();
    for y in (y0..=y1).step_by(step) {
        for x in (x0..=x1).step_by(step) {
            if let Some(response) = response_at(x, y) {
                responses.push(response);
            }
        }
    }
    let scores: Vec<_> = responses.iter().map(|(_, score)| *score).collect();
    let baseline = median(&scores);
    let deviations: Vec<_> = scores
        .iter()
        .map(|score| (score - baseline).abs())
        .collect();
    let sigma = (1.4826 * median(&deviations)).max(0.03);
    responses.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut seeds: Vec<Point2<f64>> = Vec::new();
    for &(point, raw_score) in &responses {
        if (raw_score - baseline) / sigma < 1.5 {
            break;
        }
        if seeds
            .iter()
            .all(|other| (*other - point).norm() >= (step * 2 + 1) as f64)
        {
            seeds.push(point);
        }
        if seeds.len() == 8 {
            break;
        }
    }
    let refine_radius = step as i32 + 1;
    let mut refined = Vec::new();
    for seed in seeds {
        for y in seed.y.round() as i32 - refine_radius..=seed.y.round() as i32 + refine_radius {
            for x in seed.x.round() as i32 - refine_radius..=seed.x.round() as i32 + refine_radius {
                if let Some(response) = response_at(x, y) {
                    refined.push(response);
                }
            }
        }
    }
    refined.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut selected: Vec<Candidate> = Vec::new();
    for (px, raw_score) in refined {
        let score = (raw_score - baseline) / sigma;
        if score < 2.5 {
            break;
        }
        if selected.iter().any(|other| (other.px - px).norm() < 3.0) {
            continue;
        }
        selected.push(Candidate {
            px,
            delta: (px.x - expected.x, px.y - expected.y),
            score,
        });
        if selected.len() == 4 {
            break;
        }
    }
    selected
}

fn consensus_delta(candidates: &[Vec<Candidate>], radius: f64) -> Option<(f64, f64)> {
    let mut best = None;
    for center in candidates.iter().flatten() {
        let mut members = Vec::new();
        let mut total = 0.0;
        for site in candidates {
            if let Some(candidate) = site
                .iter()
                .filter(|candidate| {
                    (candidate.delta.0 - center.delta.0).hypot(candidate.delta.1 - center.delta.1)
                        <= radius
                })
                .max_by(|a, b| a.score.total_cmp(&b.score))
            {
                let weight = candidate.score.clamp(0.0, 12.0);
                total += weight;
                members.push((*candidate, weight));
            }
        }
        if best.as_ref().is_none_or(|(score, _, _)| total > *score) {
            let weight = members.iter().map(|(_, weight)| *weight).sum::<f64>();
            let dx = members
                .iter()
                .map(|(candidate, weight)| candidate.delta.0 * weight)
                .sum::<f64>()
                / weight.max(1e-9);
            let dy = members
                .iter()
                .map(|(candidate, weight)| candidate.delta.1 * weight)
                .sum::<f64>()
                / weight.max(1e-9);
            best = Some((total, dx, dy));
        }
    }
    best.map(|(_, dx, dy)| (dx, dy))
}

fn select_lattice(
    candidates: &[Vec<Candidate>],
    n: usize,
    pitch_px: f64,
) -> Vec<Option<Candidate>> {
    let Some(global) = consensus_delta(candidates, pitch_px * 0.18) else {
        return vec![None; candidates.len()];
    };
    let global_radius = pitch_px * 0.30;
    let mut selected: Vec<Option<Candidate>> = candidates
        .iter()
        .map(|site| {
            site.iter()
                .copied()
                .filter(|candidate| {
                    (candidate.delta.0 - global.0).hypot(candidate.delta.1 - global.1)
                        <= global_radius
                })
                .max_by(|a, b| {
                    let value = |candidate: &Candidate| {
                        candidate.score
                            - 0.8
                                * (candidate.delta.0 - global.0)
                                    .hypot(candidate.delta.1 - global.1)
                                    .powi(2)
                                / global_radius.powi(2)
                    };
                    value(a).total_cmp(&value(b))
                })
        })
        .collect();

    for _ in 0..3 {
        let previous = selected.clone();
        for index in 0..selected.len() {
            let row = index / n;
            let col = index % n;
            let mut neighbors = Vec::new();
            for (dr, dc) in [(-1_i32, 0_i32), (1, 0), (0, -1), (0, 1)] {
                let (r, c) = (row as i32 + dr, col as i32 + dc);
                if r >= 0
                    && c >= 0
                    && r < n as i32
                    && c < n as i32
                    && let Some(candidate) = previous[r as usize * n + c as usize]
                {
                    neighbors.push(candidate.delta);
                }
            }
            let local = if neighbors.len() >= 2 {
                let xs: Vec<_> = neighbors.iter().map(|delta| delta.0).collect();
                let ys: Vec<_> = neighbors.iter().map(|delta| delta.1).collect();
                (median(&xs), median(&ys))
            } else {
                global
            };
            let local_radius = pitch_px * 0.16;
            selected[index] = candidates[index]
                .iter()
                .copied()
                .filter(|candidate| {
                    (candidate.delta.0 - global.0).hypot(candidate.delta.1 - global.1)
                        <= global_radius
                        && (candidate.delta.0 - local.0).hypot(candidate.delta.1 - local.1)
                            <= local_radius
                })
                .max_by(|a, b| {
                    let value = |candidate: &Candidate| {
                        candidate.score
                            - 1.4
                                * (candidate.delta.0 - local.0)
                                    .hypot(candidate.delta.1 - local.1)
                                    .powi(2)
                                / local_radius.powi(2)
                    };
                    value(a).total_cmp(&value(b))
                });
        }
    }

    // A single feature cannot satisfy two neighboring lattice sites.
    for i in 0..selected.len() {
        let Some(a) = selected[i] else { continue };
        for j in i + 1..selected.len() {
            let Some(b) = selected[j] else { continue };
            if (a.px - b.px).norm() < pitch_px * 0.35 {
                if a.score >= b.score {
                    selected[j] = None;
                } else {
                    selected[i] = None;
                    break;
                }
            }
        }
    }
    selected
}

fn refine_center(image: &Prepared, center: Point2<f64>, radius: f64) -> Point2<f64> {
    let radius = radius.clamp(4.0, 18.0);
    let limit = radius.ceil() as i32;
    let mut core = Vec::new();
    let mut ring = Vec::new();
    for dy in -limit..=limit {
        for dx in -limit..=limit {
            let distance = (dx as f64).hypot(dy as f64);
            let value = image.value(center + nalgebra::Vector2::new(dx as f64, dy as f64));
            if distance <= radius * 0.35 {
                core.push(value);
            } else if distance >= radius * 0.75 && distance <= radius {
                ring.push(value);
            }
        }
    }
    let core_level = median(&core);
    let ring_level = median(&ring);
    let polarity = if core_level >= ring_level { 1.0 } else { -1.0 };
    let threshold = ((core_level - ring_level).abs() * 0.35).max(0.35);
    let (mut weight, mut x_sum, mut y_sum) = (0.0, 0.0, 0.0);
    for dy in -limit..=limit {
        for dx in -limit..=limit {
            if (dx as f64).hypot(dy as f64) > radius {
                continue;
            }
            let point = center + nalgebra::Vector2::new(dx as f64, dy as f64);
            let signal = polarity * (image.value(point) - ring_level);
            let w = (signal - threshold).max(0.0).powi(2);
            weight += w;
            x_sum += point.x * w;
            y_sum += point.y * w;
        }
    }
    if weight <= 1e-9 {
        return center;
    }
    let refined = Point2::new(x_sum / weight, y_sum / weight);
    if (refined - center).norm() <= radius * 0.55 {
        refined
    } else {
        center
    }
}

fn component_is_square_sized(image: &Prepared, center: Point2<f64>, pitch_px: f64) -> bool {
    let probe = (pitch_px * 0.10).ceil().clamp(3.0, 10.0) as i32;
    let mut positive = (f64::NEG_INFINITY, (0_i32, 0_i32));
    let mut negative = (f64::NEG_INFINITY, (0_i32, 0_i32));
    for dy in -probe..=probe {
        for dx in -probe..=probe {
            let point = center + nalgebra::Vector2::new(dx as f64, dy as f64);
            let value = image.value(point);
            if value > positive.0 {
                positive = (value, (dx, dy));
            }
            if -value > negative.0 {
                negative = (-value, (dx, dy));
            }
        }
    }
    let (polarity, seed_offset, signal) = if positive.0 >= negative.0 {
        (1.0, positive.1, positive.0)
    } else {
        (-1.0, negative.1, negative.0)
    };
    if signal < 2.2 {
        return false;
    }

    let half = (pitch_px * 0.42).ceil().clamp(8.0, 36.0) as i32;
    let cx = center.x.round() as i32;
    let cy = center.y.round() as i32;
    let x0 = (cx - half).max(0);
    let y0 = (cy - half).max(0);
    let x1 = (cx + half).min(image.width as i32 - 1);
    let y1 = (cy + half).min(image.height as i32 - 1);
    let width = (x1 - x0 + 1) as usize;
    let height = (y1 - y0 + 1) as usize;
    let seed = (cx + seed_offset.0, cy + seed_offset.1);
    let mut seen = vec![false; width * height];
    let mut stack = vec![seed];
    let (mut count, mut min_x, mut min_y) = (0usize, seed.0, seed.1);
    let (mut max_x, mut max_y) = (seed.0, seed.1);
    let max_extent = (pitch_px * 0.34).ceil().max(6.0);
    let max_area = (max_extent * max_extent * 0.80).ceil() as usize;
    while let Some((x, y)) = stack.pop() {
        if x < x0 || y < y0 || x > x1 || y > y1 {
            continue;
        }
        let local = (y - y0) as usize * width + (x - x0) as usize;
        if seen[local] {
            continue;
        }
        seen[local] = true;
        let value = image.contrast[y as usize * image.width + x as usize];
        if polarity * value < 2.2 {
            continue;
        }
        count += 1;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
        if count > max_area
            || (max_x - min_x + 1) as f64 > max_extent
            || (max_y - min_y + 1) as f64 > max_extent
        {
            return false;
        }
        for dy in -1..=1 {
            for dx in -1..=1 {
                if dx != 0 || dy != 0 {
                    stack.push((x + dx, y + dy));
                }
            }
        }
    }
    count >= 6
}

pub(super) fn detect_square_grid(
    frame: &GrayImage,
    seed: &Homography,
    grid: &GridSpec,
    dot_mm: f64,
) -> Vec<DotPair> {
    if frame.width() < 3 || frame.height() < 3 || grid.n < 2 {
        return Vec::new();
    }
    let center_mm = Point2::new(
        grid.origin_mm.0 + (grid.n.saturating_sub(1) as f64 * grid.pitch_mm) * 0.5,
        grid.origin_mm.1 + (grid.n.saturating_sub(1) as f64 * grid.pitch_mm) * 0.5,
    );
    let center_px = seed.apply(center_mm);
    let pitch_x =
        (seed.apply(Point2::new(center_mm.x + grid.pitch_mm, center_mm.y)) - center_px).norm();
    let pitch_y =
        (seed.apply(Point2::new(center_mm.x, center_mm.y + grid.pitch_mm)) - center_px).norm();
    let pitch_px = 0.5 * (pitch_x + pitch_y);
    if !pitch_px.is_finite() || pitch_px < 4.0 {
        return Vec::new();
    }
    let prepared = Prepared::new(frame, (pitch_px * 0.30).round().max(6.0) as usize);
    let points: Vec<_> = grid
        .points()
        .into_iter()
        .map(|(x, y)| Point2::new(x, y))
        .collect();
    let candidates: Vec<_> = points
        .iter()
        .map(|&mm| site_candidates(&prepared, seed, mm, grid, dot_mm, pitch_px * 0.38))
        .collect();
    select_lattice(&candidates, grid.n, pitch_px)
        .into_iter()
        .zip(points)
        .filter_map(|(candidate, mm)| {
            candidate.map(|candidate| (refine_center(&prepared, candidate.px, pitch_px * 0.18), mm))
        })
        .filter(|(center, _)| component_is_square_sized(&prepared, *center, pitch_px))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRUTH: [[(f64, f64); 7]; 7] = [
        [
            (134.4, 72.2),
            (195.9, 65.9),
            (256.4, 60.8),
            (317.4, 56.3),
            (377.5, 51.4),
            (437.3, 46.9),
            (494.8, 41.2),
        ],
        [
            (130.0, 126.2),
            (195.9, 120.1),
            (260.9, 114.0),
            (325.6, 108.3),
            (389.7, 102.1),
            (452.9, 96.8),
            (515.2, 90.3),
        ],
        [
            (128.0, 186.8),
            (198.2, 180.0),
            (266.5, 172.4),
            (334.6, 165.2),
            (402.1, 158.8),
            (468.6, 152.0),
            (533.8, 145.1),
        ],
        [
            (130.4, 248.8),
            (202.2, 241.0),
            (273.6, 232.8),
            (344.0, 225.5),
            (413.6, 218.4),
            (482.9, 210.5),
            (550.0, 203.3),
        ],
        [
            (135.2, 314.8),
            (209.3, 305.2),
            (282.0, 296.6),
            (354.5, 288.1),
            (425.6, 281.0),
            (496.2, 272.7),
            (565.2, 265.0),
        ],
        [
            (144.0, 384.5),
            (218.2, 374.5),
            (292.7, 365.5),
            (365.2, 357.1),
            (437.1, 348.3),
            (508.2, 339.2),
            (578.1, 331.5),
        ],
        [
            (157.3, 456.7),
            (230.3, 445.4),
            (303.7, 435.8),
            (376.7, 427.5),
            (448.0, 418.4),
            (519.1, 409.7),
            (587.9, 400.6),
        ],
    ];

    #[test]
    fn live_burn_grid_locks_as_one_coherent_lattice() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/burn-grid-live-7x7.png"
        );
        let frame = image::open(path).unwrap().to_luma8();
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        let corners = [TRUTH[6][0], TRUTH[6][6], TRUTH[0][6], TRUTH[0][0]];
        let calibration = super::super::fit_camera_to_machine(
            &frame,
            corners,
            &grid,
            0.5,
            super::super::DotKind::Bright,
        )
        .expect("the live bright-square frame fits through the operator path");
        let pairs: Vec<DotPair> = calibration
            .dots
            .iter()
            .map(|dot| {
                (
                    Point2::new(dot.px.0, dot.px.1),
                    Point2::new(dot.mm.0, dot.mm.1),
                )
            })
            .collect();
        let mut errors = Vec::new();
        for (px, mm) in &pairs {
            let col = (mm.x / 10.0).round() as usize;
            let row_from_bottom = (mm.y / 10.0).round() as usize;
            let row = 6 - row_from_bottom;
            // The central square is fully hidden by glare; it is allowed to
            // remain unresolved rather than forcing a false center.
            if row == 3 && col == 3 {
                continue;
            }
            let truth = TRUTH[row][col];
            errors.push((px.x - truth.0).hypot(px.y - truth.1));
        }
        errors.sort_by(f64::total_cmp);
        if let Ok(directory) = std::env::var("PCBFORGE_DUMP_CALIB") {
            eprintln!(
                "live grid: {}/49 detected, median {:.2}px, {}/{} within 3px",
                pairs.len(),
                errors[errors.len() / 2],
                errors.iter().filter(|&&error| error <= 3.0).count(),
                errors.len()
            );
            let directory = std::path::Path::new(&directory);
            std::fs::create_dir_all(directory).unwrap();
            let prepared = Prepared::new(&frame, 20);
            prepared
                .preview()
                .save(directory.join("burn-grid-adaptive-contrast.png"))
                .unwrap();
            let mut overlay = image::DynamicImage::ImageLuma8(frame).to_rgb8();
            for (px, _) in &pairs {
                let (cx, cy) = (px.x.round() as i32, px.y.round() as i32);
                for d in -5..=5 {
                    for (x, y) in [(cx + d, cy), (cx, cy + d)] {
                        if x >= 0
                            && y >= 0
                            && x < overlay.width() as i32
                            && y < overlay.height() as i32
                        {
                            overlay.put_pixel(x as u32, y as u32, image::Rgb([40, 220, 80]));
                        }
                    }
                }
            }
            for row in TRUTH {
                for (x, y) in row {
                    let (cx, cy) = (x.round() as i32, y.round() as i32);
                    for d in -3..=3 {
                        for (x, y) in [(cx + d, cy + d), (cx + d, cy - d)] {
                            if x >= 0
                                && y >= 0
                                && x < overlay.width() as i32
                                && y < overlay.height() as i32
                            {
                                overlay.put_pixel(x as u32, y as u32, image::Rgb([40, 210, 240]));
                            }
                        }
                    }
                }
            }
            overlay
                .save(directory.join("burn-grid-detections.png"))
                .unwrap();
        }
        assert!(pairs.len() >= 48, "locked only {}/49 sites", pairs.len());
        assert!(
            errors[errors.len() / 2] <= 1.0,
            "median center error {:.2}px",
            errors[errors.len() / 2]
        );
        assert!(
            errors.iter().filter(|&&error| error <= 3.0).count() >= 47,
            "only {}/{} centers are within 3px",
            errors.iter().filter(|&&error| error <= 3.0).count(),
            errors.len()
        );
    }
}
