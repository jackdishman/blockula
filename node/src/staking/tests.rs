use super::*;

#[test]
fn bps_render_as_percentages() {
    assert_eq!((fmt_bps(250), fmt_bps(500), fmt_bps(1), fmt_bps(10_000)), ("2.5".into(), "5".into(), "0.01".into(), "100".into()));
}
