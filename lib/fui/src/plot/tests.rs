// SPDX-License-Identifier: Apache-2.0
//! Every figure here is what the plot kit's own JavaScript answers for the
//! same data (run with Node on `kit/core/plot.js`, 2026-11-18): the port is
//! the kit's pen, number for number.

use super::*;

fn p(x: f64, y: f64) -> Pt {
    Pt { x, y }
}

/// The kit writes each figure to one place (`toFixed(1)`).
fn one(v: f64) -> f64 {
    let r = ((v * 10.0) + if v >= 0.0 { 0.5 } else { -0.5 }) as i64 as f64 / 10.0;
    if r == 0.0 { 0.0 } else { r }
}

#[test]
fn the_smooth_curve_is_the_kits() {
    let pts = [p(0., 80.), p(20., 40.), p(40., 42.), p(60., 10.), p(80., 70.), p(100., 70.), p(120., 20.)];
    // smoothThrough: C6.7 66.7 13.3 40.0 20.0 40.0 C26.7 40.0 33.3 42.0 40.0 42.0
    // C46.7 42.0 53.3 10.0 60.0 10.0 C66.7 10.0 73.3 70.0 80.0 70.0
    // C86.7 70.0 93.3 70.0 100.0 70.0 C106.7 70.0 113.3 36.7 120.0 20.0
    let want = [
        [6.7, 66.7, 13.3, 40.0, 20.0, 40.0],
        [26.7, 40.0, 33.3, 42.0, 40.0, 42.0],
        [46.7, 42.0, 53.3, 10.0, 60.0, 10.0],
        [66.7, 10.0, 73.3, 70.0, 80.0, 70.0],
        [86.7, 70.0, 93.3, 70.0, 100.0, 70.0],
        [106.7, 70.0, 113.3, 36.7, 120.0, 20.0],
    ];
    let got: Vec<[f64; 6]> =
        smooth_through(&pts).iter().map(|s| [s.c1.x, s.c1.y, s.c2.x, s.c2.y, s.to.x, s.to.y].map(one)).collect();
    assert_eq!(got, want);
    // Fewer than two points: nothing.
    assert!(smooth_through(&[p(1., 1.)]).is_empty());
}

#[test]
fn the_curve_never_overshoots_its_points() {
    let pts = [p(0., 50.), p(10., 10.), p(20., 12.), p(30., 90.), p(40., 89.)];
    let line = flatten(&pts, 24);
    for w in pts.windows(2) {
        let (lo, hi) = (w[0].y.min(w[1].y), w[0].y.max(w[1].y));
        for q in line.iter().filter(|q| q.x >= w[0].x && q.x <= w[1].x) {
            assert!(q.y >= lo - 1e-9 && q.y <= hi + 1e-9, "{q:?} outside {lo}..{hi}");
        }
    }
}

#[test]
fn steadied_is_the_kits() {
    let v = [-60., -20., -3., -6., -9., -30., -12., -15., -40., -18., -21.];
    let want = [
        -60.0,
        -20.0,
        -3.0,
        -6.0,
        -13.5,
        -17.0,
        -19.444444444444443,
        -21.88888888888889,
        -24.333333333333332,
        -24.25,
        -21.0,
    ];
    let got = steadied(&v, 2);
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() < 1e-9, "{got:?}");
    }
}

#[test]
fn crisp_lines_and_meter_cells_are_the_kits() {
    assert_eq!(crisp(10.2, 1.0), 10.5);
    assert_eq!(crisp(10.7, 2.0), 10.75);
    assert!((crisp(3.999, 1.5) - 3.6666666666666665).abs() < 1e-12);
    // ladderCells / meterCells: {17,9} {17,1} {17,0} {1,1} {17,12} {17,0} {17,17}
    assert_eq!(ladder_cells(100, Some(0.5)), (17, 9));
    assert_eq!(ladder_cells(100, Some(0.001)), (17, 1));
    assert_eq!(ladder_cells(100, Some(0.0)), (17, 0));
    assert_eq!(ladder_cells(3, Some(1.0)), (1, 1));
    assert_eq!(meter_cells(100, Some(-18.0)), (17, 12));
    assert_eq!(meter_cells(100, Some(-90.0)), (17, 0));
    assert_eq!(meter_cells(100, Some(0.0)), (17, 17));
    assert_eq!(ladder_cells(100, None), (17, 0));
    // ladderCellY(100, 0) = 96, (100, 5) = 66.
    assert_eq!(ladder_cell_y(100, 0), 96);
    assert_eq!(ladder_cell_y(100, 5), 66);
}

#[test]
fn the_inks_fall_as_the_kits() {
    // INK_STOPS body {top .72, mid .4, foot .1}, soft {.36, .2, .05}; INK_MID .55.
    assert_eq!(Ink::Body.stops(), (0.72, 0.4, 0.1));
    assert_eq!(Ink::Soft.stops(), (0.36, 0.2, 0.05));
    assert!((Ink::Body.at(0.0, 0.0, 100.0) - 0.72).abs() < 1e-12);
    assert!((Ink::Body.at(55.0, 0.0, 100.0) - 0.4).abs() < 1e-12);
    assert!((Ink::Body.at(100.0, 0.0, 100.0) - 0.1).abs() < 1e-12);
    // By level: two heights, two strengths; the same height, the same strength.
    assert!(Ink::Body.at(20.0, 0.0, 100.0) > Ink::Body.at(80.0, 0.0, 100.0));
}

#[test]
fn the_light_is_strong_up_high_and_nothing_is_drawn_without_data() {
    let mut px = alloc::vec![0u32; 100 * 100];
    let mut s = Surface { px: &mut px, width: 100, height: 100, stride: 100 };
    let green = Rgba { r: 0x79, g: 0xD9, b: 0x20, a: 255 };
    // One point: nothing at all.
    fill_under(&mut s, &[p(10., 10.)], 0.0, 100.0, green, Ink::Body);
    assert!(s.px.iter().all(|&c| c == 0));
    fill_under(&mut s, &[p(0., 10.), p(100., 10.)], 0.0, 100.0, green, Ink::Body);
    let g = |x: usize, y: usize| (s.px[y * 100 + x] >> 8) & 0xFF;
    assert!(g(50, 12) > g(50, 90), "brighter near the top: {} vs {}", g(50, 12), g(50, 90));
    assert_eq!(s.px[5 * 100 + 50], 0, "nothing above the line");
}
