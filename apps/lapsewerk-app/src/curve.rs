//! The brightness curve: the sequence's brightness frame by frame (as shot, in the video, the
//! scene's light), the keyframes, and the frame looked at. Click or drag to move through the
//! sequence; hover to read the values.

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Vec2};

use crate::theme;

pub struct Series<'a> {
    pub values: &'a [Option<f64>],
    pub color: Color32,
    pub label: &'a str,
    pub width: f32,
}

pub struct Curve<'a> {
    pub series: Vec<Series<'a>>,
    pub frames: usize,
    pub keyframes: &'a [usize],
    pub suggested: &'a [usize],
    /// Frames to warn about (odd frames, replaced): red marks along the bottom.
    pub warnings: &'a [usize],
    pub height: f32,
    pub stops_word: &'a str,
    pub frame_word: &'a str,
    /// Said when hovering a suggestion's ring along the top (what the rings are).
    pub suggested_word: &'a str,
    /// The rings' entry in the legend.
    pub suggested_legend: &'a str,
}

impl Curve<'_> {
    /// Draws the curve; moves `cursor` when clicked or dragged. Returns whether it moved.
    pub fn show(&self, ui: &mut egui::Ui, cursor: &mut usize) -> bool {
        let width = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, self.height), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 12.0, theme::SURFACE);
        let n = self.frames.max(1);
        // A header row (axis title, legend), then the plot with its keyframe marks on top. A
        // legend that does not fit beside the title (a narrow window, the help open) goes on a
        // second row.
        let width_of = |t: &str| {
            painter
                .layout_no_wrap(t.to_owned(), FontId::proportional(11.5), theme::MUTED)
                .size()
                .x
        };
        // The rings along the top get a legend entry when there are any and it fits beside the
        // axis title (in a narrow curve the rings explain themselves on hover).
        let rings = self.suggested.iter().any(|k| !self.keyframes.contains(k))
            && width_of(self.stops_word) + width_of(self.suggested_legend) + 60.0 < rect.width();
        let legend_w: f32 = self.series.iter().map(|s| width_of(s.label) + 30.0).sum::<f32>()
            + if rings {
                width_of(self.suggested_legend) + 30.0
            } else {
                0.0
            };
        let two_rows = width_of(self.stops_word) + 12.0 + 24.0 + legend_w > rect.width() - 16.0;
        let legend_top = if two_rows {
            rect.top() + 25.0
        } else {
            rect.top() + 9.0
        };
        let plot = Rect::from_min_max(
            rect.min + Vec2::new(46.0, if two_rows { 56.0 } else { 40.0 }),
            rect.max - Vec2::new(16.0, 26.0),
        );

        // Range of all finite values, rounded out to whole stops (at least two stops tall).
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for s in &self.series {
            for v in s.values.iter().flatten().filter(|v| v.is_finite()) {
                lo = lo.min(*v);
                hi = hi.max(*v);
            }
        }
        if lo > hi {
            (lo, hi) = (-1.0, 1.0);
        }
        let (lo, hi) = {
            let (l, h) = (lo.floor(), hi.ceil());
            if h - l < 2.0 { (l - 1.0, h + 1.0) } else { (l, h) }
        };
        let x_of = |i: f64| plot.left() + (i / (n.saturating_sub(1).max(1)) as f64) as f32 * plot.width();
        let y_of = |v: f64| plot.bottom() - ((v - lo) / (hi - lo)) as f32 * plot.height();

        // Grid: one line per stop (every second when tall), labels on the left.
        let step = grid_step(hi - lo);
        let mut s = lo;
        while s <= hi + 1e-9 {
            let y = y_of(s);
            painter.line_segment(
                [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
                Stroke::new(1.0, theme::LINE),
            );
            painter.text(
                Pos2::new(plot.left() - 8.0, y),
                Align2::RIGHT_CENTER,
                format!("{s:+.0}"),
                FontId::proportional(11.0),
                theme::FAINT,
            );
            s += step;
        }
        painter.text(
            Pos2::new(rect.left() + 12.0, rect.top() + 10.0),
            Align2::LEFT_TOP,
            self.stops_word,
            FontId::proportional(11.5),
            theme::FAINT,
        );
        // Frame numbers along the bottom.
        let ticks = 6usize.min(n);
        for k in 0..ticks {
            let i = k * (n - 1) / (ticks - 1).max(1);
            painter.text(
                Pos2::new(x_of(i as f64), plot.bottom() + 6.0),
                Align2::CENTER_TOP,
                (i + 1).to_string(),
                FontId::proportional(11.0),
                theme::FAINT,
            );
        }

        // The series, as lines broken where a frame was not measured.
        for s in &self.series {
            let mut run: Vec<Pos2> = Vec::new();
            for (i, v) in s.values.iter().enumerate() {
                match v.filter(|v| v.is_finite()) {
                    Some(v) => run.push(Pos2::new(x_of(i as f64), y_of(v))),
                    None => {
                        if run.len() > 1 {
                            painter.add(Shape::line(
                                std::mem::take(&mut run),
                                Stroke::new(s.width, s.color),
                            ));
                        }
                        run.clear();
                    }
                }
            }
            if run.len() > 1 {
                painter.add(Shape::line(run, Stroke::new(s.width, s.color)));
            }
        }

        // Keyframes (filled diamonds) and suggestions (rings) along the top.
        let mark_y = plot.top() - 14.0;
        for &k in self.suggested.iter().filter(|k| !self.keyframes.contains(k)) {
            painter.circle_stroke(
                Pos2::new(x_of(k as f64), mark_y + 4.0),
                4.0,
                Stroke::new(1.5, theme::ACCENT),
            );
        }
        for &k in self.keyframes {
            let c = Pos2::new(x_of(k as f64), mark_y + 4.0);
            let d = 5.0;
            painter.add(Shape::convex_polygon(
                vec![
                    c + Vec2::new(0.0, -d),
                    c + Vec2::new(d, 0.0),
                    c + Vec2::new(0.0, d),
                    c + Vec2::new(-d, 0.0),
                ],
                theme::ACCENT,
                Stroke::NONE,
            ));
            painter.line_segment(
                [Pos2::new(c.x, plot.top() + 6.0), Pos2::new(c.x, plot.bottom())],
                Stroke::new(1.0, theme::ACCENT_DIM),
            );
        }

        // Warnings: small red bars on the bottom line.
        for &w in self.warnings {
            let x = x_of(w as f64);
            painter.line_segment(
                [Pos2::new(x, plot.bottom() - 10.0), Pos2::new(x, plot.bottom())],
                Stroke::new(2.5, theme::BAD),
            );
        }

        // Cursor.
        let cur = (*cursor).min(n - 1);
        let cx = x_of(cur as f64);
        painter.line_segment(
            [Pos2::new(cx, plot.top()), Pos2::new(cx, plot.bottom())],
            Stroke::new(1.5, theme::TEXT),
        );

        // Legend, top right.
        let mut x = plot.right();
        for s in self.series.iter().rev() {
            let galley = painter.layout_no_wrap(s.label.to_owned(), FontId::proportional(11.5), theme::MUTED);
            x -= galley.size().x;
            painter.galley(Pos2::new(x, legend_top), galley, theme::MUTED);
            x -= 16.0;
            painter.line_segment(
                [
                    Pos2::new(x, legend_top + 7.0),
                    Pos2::new(x + 12.0, legend_top + 7.0),
                ],
                Stroke::new(2.5, s.color),
            );
            x -= 14.0;
        }
        if rings {
            // On two rows it goes top right, beside the axis title (the lines' row is full).
            let (mut x, top) = if two_rows {
                (plot.right(), rect.top() + 9.0)
            } else {
                (x, legend_top)
            };
            let galley = painter.layout_no_wrap(
                self.suggested_legend.to_owned(),
                FontId::proportional(11.5),
                theme::MUTED,
            );
            x -= galley.size().x;
            painter.galley(Pos2::new(x, top), galley, theme::MUTED);
            painter.circle_stroke(
                Pos2::new(x - 9.0, top + 7.0),
                4.0,
                Stroke::new(1.5, theme::ACCENT),
            );
        }

        // Moving through the sequence.
        let to_frame = |p: Pos2| {
            (((p.x - plot.left()) / plot.width()).clamp(0.0, 1.0) * (n - 1) as f32).round() as usize
        };
        let mut moved = false;
        if (response.clicked() || response.dragged())
            && let Some(p) = response.interact_pointer_pos()
        {
            let f = to_frame(p);
            if f != *cursor {
                *cursor = f;
                moved = true;
            }
        }
        if let Some(p) = response.hover_pos() {
            let f = to_frame(p);
            let hx = x_of(f as f64);
            painter.line_segment(
                [Pos2::new(hx, plot.top()), Pos2::new(hx, plot.bottom())],
                Stroke::new(1.0, theme::FAINT),
            );
            let mut tip = format!("{} {}", self.frame_word, f + 1);
            if self.keyframes.contains(&f) {
                tip.push_str(" · Keyframe");
            } else if self.suggested.contains(&f) {
                tip.push_str(&format!(" · {}", self.suggested_word));
            }
            for s in &self.series {
                if let Some(Some(v)) = s.values.get(f) {
                    tip.push_str(&format!("\n{}: {v:+.2}", s.label));
                }
            }
            response.on_hover_text_at_pointer(tip);
        }
        moved
    }
}

/// One grid line per stop, every second when tall, and at most about a dozen lines whatever the
/// range (values from a damaged file must not draw a billion of them).
fn grid_step(range: f64) -> f64 {
    if !range.is_finite() || range > 24.0 {
        (range / 12.0).ceil().clamp(2.0, 1e12)
    } else if range > 8.0 {
        2.0
    } else {
        1.0
    }
}

/// The words of [`shape_editor`].
pub struct ShapeWords<'a> {
    pub frame: &'a str,
    /// The axis: the video's brightness, in stops.
    pub axis: &'a str,
    pub with_points: &'a str,
    pub without: &'a str,
    /// "Bl." / "st.": after a point's change.
    pub stops_short: &'a str,
    /// Saved keyframes (frames from 0): diamonds along the top, as on the other curves.
    pub keyframes: &'a [usize],
    /// The frame where a point would go (a line down the plot).
    pub cursor: Option<usize>,
    /// The light as shot (the camera's steps undone, nothing smoothed), drawn thin beside the
    /// curves where it fits: where the moon really rises. Empty: not drawn.
    pub light: &'a [Option<f64>],
    pub light_word: &'a str,
}

/// The video's brightness, shaped by hand (like a holy-grail curve): `video` the brightness as the
/// video has it now (stops, per frame, the shape included), `base` without the shape (it sets the
/// scale, so the view holds still while dragging), `points` the shape's (frame, stops). Click on
/// the curve to add a point, drag a point up or down, double-click it to take it away. Returns
/// whether the points changed.
/// Puts `points` in frame order on frames `0..frames`, a point past the end onto the last frame,
/// keeping the first of points on one frame (as the command line does). Whether anything changed.
pub fn tidy(points: &mut Vec<(usize, f64)>, frames: usize) -> bool {
    let before = points.clone();
    let last = frames.saturating_sub(1);
    for p in points.iter_mut() {
        p.0 = p.0.min(last);
    }
    // Stable: of points on one frame, the one given first stays first.
    points.sort_by_key(|p| p.0);
    points.dedup_by_key(|p| p.0);
    *points != before
}

pub fn shape_editor(
    ui: &mut egui::Ui,
    base: &[Option<f64>],
    video: &[Option<f64>],
    points: &mut Vec<(usize, f64)>,
    height: f32,
    words: &ShapeWords<'_>,
    comma: bool,
) -> bool {
    let frame_word = words.frame;
    let n = video.len().max(2);
    // What is shown is what renders: a point past the end (a project file edited by hand, a
    // shorter series) moves onto the last frame, and of two points on one frame the first counts.
    let tidied = video.len() >= 2 && tidy(points, video.len());
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 12.0, theme::SURFACE);
    // A header row: what the axis is, and what the two lines are.
    let plot = Rect::from_min_max(rect.min + Vec2::new(46.0, 34.0), rect.max - Vec2::new(16.0, 24.0));
    painter.text(
        Pos2::new(rect.left() + 12.0, rect.top() + 10.0),
        Align2::LEFT_TOP,
        words.axis,
        FontId::proportional(11.5),
        theme::FAINT,
    );
    let mut lx = rect.right() - 16.0;
    let mut legend = vec![
        (words.with_points, theme::CURVE_VIDEO),
        (words.without, theme::CURVE_SHOT),
    ];
    if !words.light.is_empty() {
        legend.push((words.light_word, theme::CURVE_SCENE));
    }
    for (label, color) in legend {
        let g = painter.layout_no_wrap(label.to_owned(), FontId::proportional(11.5), theme::MUTED);
        lx -= g.size().x;
        painter.galley(Pos2::new(lx, rect.top() + 9.0), g, theme::MUTED);
        lx -= 16.0;
        painter.line_segment(
            [
                Pos2::new(lx, rect.top() + 16.0),
                Pos2::new(lx + 12.0, rect.top() + 16.0),
            ],
            Stroke::new(2.5, color),
        );
        lx -= 14.0;
    }
    // The scale: the curve without the shape and with it, a stop and a half of room either way
    // (the points' room to move), so small changes show.
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for v in base.iter().chain(video).flatten().filter(|v| v.is_finite()) {
        lo = lo.min(*v);
        hi = hi.max(*v);
    }
    if lo > hi {
        (lo, hi) = (-1.0, 1.0);
    }
    let (lo, hi) = ((lo - 1.5).floor(), (hi + 1.5).ceil());
    let x_of = |i: f64| plot.left() + (i / (n - 1) as f64) as f32 * plot.width();
    let y_of = |v: f64| plot.bottom() - ((v - lo) / (hi - lo)) as f32 * plot.height();
    let px_per_stop = plot.height() / (hi - lo) as f32;
    // The saved keyframes, as diamonds on the plot's top edge with a faint line down.
    for &k in words.keyframes.iter().filter(|k| **k < n) {
        let c = Pos2::new(x_of(k as f64), plot.top() - 6.0);
        painter.line_segment(
            [Pos2::new(c.x, plot.top()), Pos2::new(c.x, plot.bottom())],
            Stroke::new(1.0, theme::ACCENT_DIM),
        );
        painter.add(egui::Shape::convex_polygon(
            vec![
                c + Vec2::new(0.0, -4.5),
                c + Vec2::new(4.5, 0.0),
                c + Vec2::new(0.0, 4.5),
                c + Vec2::new(-4.5, 0.0),
            ],
            theme::ACCENT,
            Stroke::NONE,
        ));
    }
    // Where "+ point" puts one: a bright dashed line down the plot, its frame on top.
    if let Some(c) = words.cursor.filter(|c| *c < n) {
        let x = x_of(c as f64);
        let mut y = plot.top();
        while y < plot.bottom() {
            let end = (y + 5.0).min(plot.bottom());
            painter.line_segment(
                [Pos2::new(x, y), Pos2::new(x, end)],
                Stroke::new(1.5, theme::TEXT),
            );
            y += 9.0;
        }
        let label = format!("{frame_word} {}", c + 1);
        let galley = painter.layout_no_wrap(label, FontId::proportional(11.0), theme::BG);
        let w = galley.size().x + 10.0;
        let left = (x - w / 2.0).clamp(plot.left(), plot.right() - w);
        let tag = Rect::from_min_size(Pos2::new(left, plot.top() + 2.0), Vec2::new(w, 16.0));
        painter.rect_filled(tag, 4.0, theme::TEXT);
        painter.galley(tag.min + Vec2::new(5.0, 1.5), galley, theme::BG);
    }
    let mut s = lo;
    while s <= hi + 1e-9 {
        let y = y_of(s);
        painter.line_segment(
            [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
            Stroke::new(1.0, theme::LINE),
        );
        painter.text(
            Pos2::new(plot.left() - 8.0, y),
            Align2::RIGHT_CENTER,
            format!("{s:+.0}"),
            FontId::proportional(11.0),
            theme::FAINT,
        );
        s += grid_step(hi - lo);
    }
    for k in 0..6usize {
        let i = k * (n - 1) / 5;
        painter.text(
            Pos2::new(x_of(i as f64), plot.bottom() + 6.0),
            Align2::CENTER_TOP,
            (i + 1).to_string(),
            FontId::proportional(11.0),
            theme::FAINT,
        );
    }
    // The light as shot, laid onto the curve without the shape (their median difference), thin:
    // where the light really changes, against where the video changes.
    let mut gaps: Vec<f64> = base
        .iter()
        .zip(words.light)
        .filter_map(|(b, l)| Some(b.as_ref()? - l.as_ref()?))
        .filter(|d| d.is_finite())
        .collect();
    gaps.sort_by(f64::total_cmp);
    let light: Vec<Option<f64>> = match gaps.get(gaps.len() / 2) {
        Some(m) => words.light.iter().map(|l| l.map(|l| l + m)).collect(),
        None => Vec::new(),
    };
    // Without the shape (thin), and with it.
    for (values, color, w) in [
        (light.as_slice(), theme::CURVE_SCENE, 1.0),
        (base, theme::CURVE_SHOT, 1.2),
        (video, theme::CURVE_VIDEO, 2.5),
    ] {
        let line: Vec<Pos2> = values
            .iter()
            .enumerate()
            .filter_map(|(i, v)| {
                v.filter(|v| v.is_finite())
                    .map(|v| Pos2::new(x_of(i as f64), y_of(v)))
            })
            .collect();
        if line.len() > 1 {
            // Inside the plot: a curve beyond its scale (the light of a deep night) is cut at its
            // edge, not drawn over the axis words.
            painter
                .with_clip_rect(plot.expand(1.0))
                .add(Shape::line(line, Stroke::new(w, color)));
        }
    }
    let at = |f: usize| -> Option<Pos2> {
        video
            .get(f)
            .copied()
            .flatten()
            .map(|v| Pos2::new(x_of(f as f64), y_of(v)))
    };
    // Which point is held, and its value when it was taken (kept while dragging): the value
    // follows the pointer's distance from where it was pressed, so no movement is lost.
    let held_id = response.id.with("held");
    let mut held: Option<(usize, f64)> = ui
        .ctx()
        .data(|d| d.get_temp::<Option<(usize, f64)>>(held_id))
        .flatten();
    let near = |points: &[(usize, f64)], p: Pos2| -> Option<usize> {
        points
            .iter()
            .enumerate()
            .filter_map(|(k, &(f, _))| at(f).map(|q| (k, q.distance(p))))
            .filter(|(_, d)| *d < 12.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(k, _)| k)
    };
    let mut changed = tidied;
    // A click next to a keyframe (its line or diamond) lands on the keyframe's frame.
    let keyframes = words.keyframes;
    let frame_at = |p: Pos2| -> usize {
        let f = (((p.x - plot.left()) / plot.width()).clamp(0.0, 1.0) * (n - 1) as f32).round() as usize;
        keyframes
            .iter()
            .filter(|k| **k < n)
            .map(|&k| (k, (x_of(k as f64) - p.x).abs()))
            .filter(|(_, d)| *d <= 8.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map_or(f, |(k, _)| k)
    };
    let origin = ui.input(|i| i.pointer.press_origin());
    if response.drag_started()
        && let Some(o) = origin
    {
        held = near(points, o).and_then(|k| points.get(k).map(|p| (k, p.1)));
    }
    if response.dragged()
        && let (Some((k, start)), Some(o), Some(now)) = (held, origin, response.interact_pointer_pos())
        && let Some(pt) = points.get_mut(k)
    {
        let d = f64::from((o.y - now.y) / px_per_stop.max(1e-3));
        let v = ((start + d) * 20.0).round() / 20.0;
        if (v - pt.1).abs() > 1e-9 {
            pt.1 = v.clamp(-6.0, 6.0);
            changed = true;
        }
    }
    if response.drag_stopped() {
        held = None;
    }
    if response.double_clicked()
        && let Some(p) = response.interact_pointer_pos()
        && let Some(k) = near(points, p)
    {
        points.remove(k);
        changed = true;
    } else if response.clicked()
        && let Some(p) = response.interact_pointer_pos()
        && near(points, p).is_none()
        && points.len() < 64
    {
        let f = frame_at(p);
        if !points.iter().any(|&(g, _)| g == f) {
            // The point starts where the curve is: nothing moves until it is dragged.
            let here = crate::curve::shape_at(points, f, n);
            points.push((f, here));
            points.sort_by_key(|&(g, _)| g);
            changed = true;
        }
    }
    ui.ctx().data_mut(|d| d.insert_temp(held_id, held));
    // The points: rings, the held one filled. A label too close to the one before is left out
    // (the hover names every frame); the held point's always shows.
    let mut last_label = f32::MIN;
    let mut last_below = false;
    for (k, &(f, s)) in points.iter().enumerate() {
        if let Some(q) = at(f) {
            if held.is_some_and(|(h, _)| h == k) {
                painter.circle_filled(q, 6.0, theme::ACCENT);
            } else {
                painter.circle_stroke(q, 6.0, Stroke::new(2.0, theme::ACCENT));
            }
            // Inside the plot at its ends (not over the axis labels).
            let align = if q.x < plot.left() + 20.0 {
                Align2::LEFT_BOTTOM
            } else if q.x > plot.right() - 20.0 {
                Align2::RIGHT_BOTTOM
            } else {
                Align2::CENTER_BOTTOM
            };
            let held_here = held.is_some_and(|(h, _)| h == k);
            // Too close to the label before: below the point instead (then above again).
            let below = q.x - last_label < 64.0 && !held_here && !last_below;
            if q.x - last_label < 64.0 && !held_here && last_below {
                continue;
            }
            last_below = below;
            last_label = q.x;
            // The change at this point (not a place on the axis): "+1.2 st." brighter there.
            // As set: two decimals where it has them (−0,25, not −0,2), one otherwise.
            let value = format!("{s:+.2}");
            let value = if value.ends_with('0') {
                format!("{s:+.1}")
            } else {
                value
            };
            let label = format!("{value} {}", words.stops_short);
            let (offset, align) = if below {
                // The same alignment, hanging under the point.
                let a = match align {
                    Align2::LEFT_BOTTOM => Align2::LEFT_TOP,
                    Align2::RIGHT_BOTTOM => Align2::RIGHT_TOP,
                    _ => Align2::CENTER_TOP,
                };
                (Vec2::new(0.0, 10.0), a)
            } else {
                (Vec2::new(0.0, -10.0), align)
            };
            painter.text(
                q + offset,
                align,
                if comma { label.replace('.', ",") } else { label },
                FontId::proportional(11.0),
                theme::MUTED,
            );
        }
    }
    if let Some(p) = response.hover_pos() {
        let f = frame_at(p);
        let point = points.iter().find(|q| q.0 == f).map(|q| q.1);
        let text = match point {
            Some(v) => {
                let v = format!("{v:+.2} {}", words.stops_short);
                format!(
                    "{frame_word} {}: {}",
                    f + 1,
                    if comma { v.replace('.', ",") } else { v }
                )
            }
            None => format!("{frame_word} {}", f + 1),
        };
        response.on_hover_text(text);
    }
    changed
}

/// The shape's value at `frame`, as the video gets it.
#[must_use]
pub fn shape_at(points: &[(usize, f64)], frame: usize, n: usize) -> f64 {
    lapsewerk_core::daynight::shaped(points, n)
        .get(frame)
        .copied()
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_are_shown_where_they_render() {
        let mut p = vec![(40, 1.2), (15, 0.5), (15, 0.8), (3, -0.2)];
        assert!(tidy(&mut p, 30));
        assert_eq!(p, [(3, -0.2), (15, 0.5), (29, 1.2)]);
        assert!(!tidy(&mut p, 30));
    }

    /// Runs one frame of the editor (600 × 200 at the origin) with `events`.
    fn frame(ctx: &egui::Context, events: Vec<egui::Event>, points: &mut Vec<(usize, f64)>, t: f64) {
        let base: Vec<Option<f64>> = (0..60).map(|i| Some(-3.0 - f64::from(i) / 30.0)).collect();
        let shape = lapsewerk_core::daynight::shaped(points, 60);
        let video: Vec<Option<f64>> = base.iter().zip(&shape).map(|(b, s)| b.map(|b| b + s)).collect();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 200.0))),
            time: Some(t),
            events,
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    let words = ShapeWords {
                        frame: "Frame",
                        axis: "Brightness",
                        with_points: "with",
                        without: "without",
                        stops_short: "st.",
                        keyframes: &[],
                        cursor: None,
                        light: &[],
                        light_word: "",
                    };
                    shape_editor(ui, &base, &video, points, 200.0, &words, false);
                });
        });
        out.textures_delta.clear();
    }

    fn press(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn click_adds_a_point_drag_raises_it_double_click_removes_it() {
        let ctx = egui::Context::default();
        let mut points = Vec::new();
        frame(&ctx, vec![], &mut points, 0.0);
        // The plot spans x 46..584; frame 30 of 60 sits about halfway, on the curve (y about 95).
        let x = 46.0 + 538.0 * 30.0 / 59.0;
        let on_curve = |points: &Vec<(usize, f64)>| {
            // Where the curve is drawn for frame 30, scaled as the editor scales it.
            let shape = lapsewerk_core::daynight::shaped(points, 60);
            let base: Vec<f64> = (0..60).map(|i| -3.0 - f64::from(i) / 30.0).collect();
            let all: Vec<f64> = base
                .iter()
                .chain(
                    base.iter()
                        .zip(&shape)
                        .map(|(b, s)| b + s)
                        .collect::<Vec<_>>()
                        .iter(),
                )
                .copied()
                .collect();
            let lo = (all.iter().copied().fold(f64::MAX, f64::min) - 1.5).floor();
            let hi = (all.iter().copied().fold(f64::MIN, f64::max) + 1.5).ceil();
            let v = base[30] + shape[30];
            let (plot_top, plot_bottom) = (34.0f32, 200.0f32 - 24.0);
            Pos2::new(
                x,
                plot_bottom - ((v - lo) / (hi - lo)) as f32 * (plot_bottom - plot_top),
            )
        };
        let p = on_curve(&points);
        frame(&ctx, vec![egui::Event::PointerMoved(p)], &mut points, 0.1);
        frame(&ctx, vec![press(p, true)], &mut points, 0.2);
        frame(&ctx, vec![press(p, false)], &mut points, 0.25);
        assert_eq!(points.len(), 1, "{points:?}");
        assert_eq!(points[0].0, 30);
        // Dragged 40 px up: brighter there.
        let p = on_curve(&points);
        frame(&ctx, vec![egui::Event::PointerMoved(p)], &mut points, 1.0);
        frame(&ctx, vec![press(p, true)], &mut points, 1.1);
        for k in 1..=4 {
            let q = p - Vec2::new(0.0, 10.0 * k as f32);
            frame(
                &ctx,
                vec![egui::Event::PointerMoved(q)],
                &mut points,
                1.1 + 0.05 * f64::from(k),
            );
        }
        frame(
            &ctx,
            vec![press(p - Vec2::new(0.0, 40.0), false)],
            &mut points,
            1.5,
        );
        assert!(points[0].1 > 1.0, "{points:?}");
        // A double click on it: gone.
        let p = on_curve(&points);
        frame(&ctx, vec![egui::Event::PointerMoved(p)], &mut points, 3.0);
        for t in [3.05, 3.1, 3.15, 3.2] {
            frame(&ctx, vec![press(p, t == 3.05 || t == 3.15)], &mut points, t);
        }
        assert!(points.is_empty(), "{points:?}");
    }
}
