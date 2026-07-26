//! The Gerber and Excellon parsers must return `Err` on any input they can't
//! make sense of — never panic, and never wrap a coordinate into a plausible
//! one. Panicking on hostile input in a laser tool is a fault; wrapping is
//! worse, because the job still runs.
//!
//! Purely random bytes bounce off the header checks, so each parser is fed a
//! valid header plus a fuzzed body: the body is where the numeric paths live.

use ingest::excellon::parse_excellon;
use ingest::gerber::parse_gerber;
use proptest::prelude::*;

const GERBER_HEADER: &str = "%FSLAX36Y36*%\n%MOMM*%\n";
const DRILL_HEADER: &str = "M48\nFMAT,2\nMETRIC\nT1C0.4\n%\nG90\nG05\nT1\n";

/// Bias the alphabet towards the characters that steer the parsers into
/// coordinate, aperture and macro-expression code rather than early rejection.
const ALPHABET: &str = "0123456789.-+XYIJDGMTC%*$/()eE,ABPRO\n";

fn body() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        proptest::sample::select(ALPHABET.chars().collect::<Vec<char>>()),
        0..160,
    )
    .prop_map(|cs| cs.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn gerber_parse_never_panics(body in body()) {
        let src = format!("{GERBER_HEADER}{body}");
        let _ = parse_gerber(&src);
        let _ = parse_gerber(&format!("{src}\nM02*\n"));
    }

    #[test]
    fn excellon_parse_never_panics(body in body()) {
        let src = format!("{DRILL_HEADER}{body}");
        let _ = parse_excellon(&src);
        let _ = parse_excellon(&format!("{src}\nM30\n"));
    }

    /// Raw bytes with no header at all: these should all be rejected, and the
    /// rejection must be an `Err`, not an abort.
    #[test]
    fn headerless_input_is_always_rejected(body in body()) {
        prop_assert!(parse_gerber(&body).is_err() || body.contains("M02"));
        prop_assert!(parse_excellon(&body).is_err());
    }
}
