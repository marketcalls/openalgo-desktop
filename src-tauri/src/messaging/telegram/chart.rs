//! The `/chart` images: a candlestick panel over a volume panel, one PNG.
//!
//! The web draws these with Plotly and renders them with Kaleido (a headless
//! Chromium). Here they are drawn into an RGB buffer and encoded with `png`,
//! with an 8x8 bitmap font, so nothing depends on system fonts or a browser
//! and the same code runs on every platform. The content matches the web's
//! chart: title, candles (green up, red down) on a category axis without
//! gaps, volume bars coloured by candle direction, about eight time labels
//! (`22 SEP 09:15` intraday, `22 SEP` daily). It is not pixel-identical.

use chrono::{DateTime, FixedOffset};

pub const WIDTH: usize = 1000;
pub const HEIGHT: usize = 600;

#[derive(Debug, Clone)]
pub struct Candle {
    pub time: DateTime<FixedOffset>,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

type Rgb = [u8; 3];

const GRID: Rgb = [235, 240, 248];
const AXIS: Rgb = [190, 196, 206];
const TEXT: Rgb = [42, 63, 95];
const GREEN: Rgb = [0, 128, 0];
const RED: Rgb = [255, 0, 0];

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            px: vec![255; w * h * 3],
        }
    }

    fn set(&mut self, x: i64, y: i64, c: Rgb) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let i = (y as usize * self.w + x as usize) * 3;
        self.px[i..i + 3].copy_from_slice(&c);
    }

    fn rect(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, c: Rgb) {
        let (xa, xb) = (x0.min(x1), x0.max(x1));
        let (ya, yb) = (y0.min(y1), y0.max(y1));
        for y in ya..=yb {
            for x in xa..=xb {
                self.set(x, y, c);
            }
        }
    }

    fn hline(&mut self, x0: i64, x1: i64, y: i64, c: Rgb) {
        self.rect(x0, y, x1, y, c);
    }

    fn vline(&mut self, x: i64, y0: i64, y1: i64, c: Rgb) {
        self.rect(x, y0, x, y1, c);
    }

    /// ASCII text, top-left at (x, y), each font pixel drawn `scale` wide.
    fn text(&mut self, x: i64, y: i64, s: &str, scale: i64, c: Rgb) {
        let mut cx = x;
        for ch in s.chars() {
            let idx = if (ch as u32) < 128 {
                ch as usize
            } else {
                b'?' as usize
            };
            let glyph = font8x8::legacy::BASIC_LEGACY[idx];
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..8 {
                    if bits & (1 << col) != 0 {
                        let px = cx + col as i64 * scale;
                        let py = y + row as i64 * scale;
                        self.rect(px, py, px + scale - 1, py + scale - 1, c);
                    }
                }
            }
            cx += 8 * scale;
        }
    }

    fn text_width(s: &str, scale: i64) -> i64 {
        s.chars().count() as i64 * 8 * scale
    }

    fn png(&self) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, self.w as u32, self.h as u32);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().ok()?;
            w.write_image_data(&self.px).ok()?;
        }
        Some(out)
    }
}

/// Round axis ticks covering [lo, hi].
fn nice_ticks(lo: f64, hi: f64, target: usize) -> Vec<f64> {
    if !(lo.is_finite() && hi.is_finite()) || hi <= lo {
        return vec![lo];
    }
    let raw = (hi - lo) / target.max(1) as f64;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0]
        .iter()
        .map(|m| m * mag)
        .find(|s| *s >= raw)
        .unwrap_or(10.0 * mag);
    let mut t = (lo / step).ceil() * step;
    let mut out = Vec::new();
    while t <= hi + step * 1e-9 && out.len() < 50 {
        out.push(t);
        t += step;
    }
    out
}

fn price_label(v: f64, step: f64) -> String {
    if step >= 1.0 {
        crate::messaging::format::comma2(v)
            .trim_end_matches("00")
            .trim_end_matches('.')
            .to_string()
    } else {
        crate::messaging::format::comma2(v)
    }
}

/// Plotly's SI tick suffixes for the volume axis.
fn si_label(v: f64) -> String {
    let a = v.abs();
    let (d, s) = if a >= 1e9 {
        (1e9, "G")
    } else if a >= 1e6 {
        (1e6, "M")
    } else if a >= 1e3 {
        (1e3, "k")
    } else {
        (1.0, "")
    };
    let n = v / d;
    let txt = if (n - n.round()).abs() < 1e-9 {
        format!("{}", n.round() as i64)
    } else {
        format!("{:.1}", n)
    };
    format!("{}{}", txt, s)
}

/// Draw the chart. `time_fmt` is the strftime of the x labels (upper-cased,
/// as the web does). `label_count` is how many labels to aim for (8 intraday,
/// 10 daily). `None` when there is nothing to draw.
pub fn render(
    candles: &[Candle],
    title: &str,
    time_fmt: &str,
    label_count: usize,
) -> Option<Vec<u8>> {
    if candles.is_empty() {
        return None;
    }
    let mut cv = Canvas::new(WIDTH, HEIGHT);
    let (left, right, top, bottom) = (90i64, 20i64, 56i64, 80i64);
    let plot_w = WIDTH as i64 - left - right;
    let plot_h = HEIGHT as i64 - top - bottom;
    let gap = (plot_h as f64 * 0.03) as i64;
    let price_h = ((plot_h - gap) as f64 * 0.7) as i64;
    let vol_top = top + price_h + gap;
    let vol_h = plot_h - price_h - gap;

    // Title, left-aligned like Plotly's subplot title, scaled 2x.
    cv.text(left, 16, title, 2, TEXT);

    let lo = candles.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
    let hi = candles
        .iter()
        .map(|c| c.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let pad = ((hi - lo) * 0.05).max(hi.abs() * 1e-4).max(1e-9);
    let (plo, phi) = (lo - pad, hi + pad);
    let y_price = |v: f64| top + ((phi - v) / (phi - plo) * price_h as f64).round() as i64;
    let vmax = candles
        .iter()
        .map(|c| c.volume)
        .fold(0.0, f64::max)
        .max(1.0)
        * 1.05;
    let y_vol = |v: f64| vol_top + ((vmax - v) / vmax * vol_h as f64).round() as i64;

    // Grids and axis labels.
    let pticks = nice_ticks(plo, phi, 6);
    let pstep = if pticks.len() > 1 {
        pticks[1] - pticks[0]
    } else {
        1.0
    };
    for t in &pticks {
        let y = y_price(*t);
        cv.hline(left, left + plot_w, y, GRID);
        let lbl = price_label(*t, pstep);
        cv.text(left - 8 - Canvas::text_width(&lbl, 1), y - 4, &lbl, 1, TEXT);
    }
    for t in nice_ticks(0.0, vmax, 3) {
        let y = y_vol(t);
        cv.hline(left, left + plot_w, y, GRID);
        let lbl = si_label(t);
        cv.text(left - 8 - Canvas::text_width(&lbl, 1), y - 4, &lbl, 1, TEXT);
    }
    cv.hline(left, left + plot_w, top + price_h, AXIS);
    cv.hline(left, left + plot_w, vol_top + vol_h, AXIS);
    cv.vline(left, top, top + price_h, AXIS);
    cv.vline(left, vol_top, vol_top + vol_h, AXIS);

    // Candles and volume on a category axis (index, no time gaps).
    let n = candles.len() as f64;
    let slot = plot_w as f64 / n;
    let half = ((slot * 0.35).floor() as i64).max(0);
    for (i, c) in candles.iter().enumerate() {
        let x = left + (slot * (i as f64 + 0.5)).round() as i64;
        let color = if c.close < c.open { RED } else { GREEN };
        cv.vline(x, y_price(c.high), y_price(c.low), color);
        let (yo, yc) = (y_price(c.open), y_price(c.close));
        cv.rect(x - half, yo, x + half, yc, color);
        cv.rect(
            x - half,
            y_vol(c.volume.max(0.0)),
            x + half,
            vol_top + vol_h,
            color,
        );
    }

    // Time labels under the volume panel, every Nth bar.
    let every = (candles.len() / label_count.max(1)).max(1);
    let mut last_end = i64::MIN;
    for i in (0..candles.len()).step_by(every) {
        let x = left + (slot * (i as f64 + 0.5)).round() as i64;
        let lbl = candles[i].time.format(time_fmt).to_string().to_uppercase();
        let w = Canvas::text_width(&lbl, 1);
        let x0 = (x - w / 2).max(0);
        if x0 <= last_end {
            continue;
        }
        cv.vline(x, vol_top + vol_h, vol_top + vol_h + 4, AXIS);
        cv.text(x0, vol_top + vol_h + 10, &lbl, 1, TEXT);
        last_end = x0 + w + 6;
    }
    cv.png()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn candles(n: usize) -> Vec<Candle> {
        let ist = FixedOffset::east_opt(19800).unwrap();
        (0..n)
            .map(|i| {
                let o = 100.0 + i as f64;
                Candle {
                    time: ist.with_ymd_and_hms(2026, 9, 22, 9, 15, 0).unwrap()
                        + chrono::Duration::minutes(5 * i as i64),
                    open: o,
                    high: o + 2.0,
                    low: o - 2.0,
                    close: if i % 2 == 0 { o + 1.0 } else { o - 1.0 },
                    volume: 1000.0 * (i + 1) as f64,
                }
            })
            .collect()
    }

    #[test]
    fn renders_a_png_with_both_colours() {
        let png = render(&candles(40), "SBIN - 5 Day Intraday (5m)", "%d %b %H:%M", 8).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let dec = png::Decoder::new(std::io::Cursor::new(png));
        let mut r = dec.read_info().unwrap();
        let mut buf = vec![0; r.output_buffer_size()];
        let info = r.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (WIDTH as u32, HEIGHT as u32));
        let px: Vec<&[u8]> = buf[..info.buffer_size()].chunks(3).collect();
        assert!(px.iter().any(|p| *p == GREEN));
        assert!(px.iter().any(|p| *p == RED));
        assert!(px.iter().any(|p| *p == TEXT));
    }

    #[test]
    fn single_candle_and_empty() {
        assert!(render(&candles(1), "X", "%d %b", 10).is_some());
        assert!(render(&[], "X", "%d %b", 10).is_none());
    }

    #[test]
    fn ticks_and_labels() {
        assert_eq!(
            nice_ticks(0.0, 10.0, 5),
            vec![0.0, 2.0, 4.0, 6.0, 8.0, 10.0]
        );
        assert_eq!(si_label(1_500_000.0), "1.5M");
        assert_eq!(si_label(300_000.0), "300k");
        assert_eq!(price_label(1200.0, 50.0), "1,200");
    }
}
