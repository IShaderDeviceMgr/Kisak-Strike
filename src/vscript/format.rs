//! The C `printf` conversions Squirrel leans on: `%g` for every float it turns
//! into text (`SQVM::ToString`), `%.14g` for error messages
//! (`PrintObjVal`), and the whole family for the `format()` library function
//! (`sqstdstring.cpp`'s `sqstd_format`, which hands each conversion to the C
//! library).
//!
//! **The exponent has at least two digits**, which is C99 and every POSIX
//! libc. Valve's Windows build linked a pre-2015 MSVC runtime, whose `%e` and
//! `%g` print three (`1e+020`); the Mac and Linux builds of Portal 2 printed
//! two, and this port is POSIX-only (`PORTING.md`).

/// The flags, width and precision of one conversion.
#[derive(Clone, Copy, Debug, Default)]
pub struct Spec {
    pub left: bool,
    pub plus: bool,
    pub space: bool,
    pub alt: bool,
    pub zero: bool,
    pub width: usize,
    pub precision: Option<usize>,
}

/// `%e`'s digits: a mantissa with `precision` decimals, and the exponent, for
/// a finite non-negative `v`.
fn exp_parts(v: f64, precision: usize) -> (String, i32) {
    let s = format!("{:.*e}", precision, v);
    let (mantissa, exp) = s.split_once('e').expect("{:e} always has an exponent");
    (mantissa.to_owned(), exp.parse().expect("{:e}'s exponent is an integer"))
}

fn exp_suffix(exp: i32, upper: bool) -> String {
    let sign = if exp < 0 { '-' } else { '+' };
    let e = if upper { 'E' } else { 'e' };
    format!("{e}{sign}{:02}", exp.unsigned_abs())
}

/// Removes trailing zeros after a decimal point, and the point if nothing is
/// left after it — `%g` without `#`.
fn strip_zeros(s: &str) -> String {
    if !s.contains('.') {
        return s.to_owned();
    }
    let trimmed = s.trim_end_matches('0');
    trimmed.trim_end_matches('.').to_owned()
}

/// The digits of a finite, non-negative `v` under one float conversion, with
/// no sign and no padding.
fn float_body(v: f64, conv: u8, spec: &Spec) -> String {
    let upper = conv.is_ascii_uppercase();
    match conv.to_ascii_lowercase() {
        b'f' => {
            let p = spec.precision.unwrap_or(6);
            let mut s = format!("{:.*}", p, v);
            if spec.alt && p == 0 {
                s.push('.');
            }
            s
        }
        b'e' => {
            let p = spec.precision.unwrap_or(6);
            let (mut mantissa, exp) = exp_parts(v, p);
            if spec.alt && p == 0 {
                mantissa.push('.');
            }
            mantissa + &exp_suffix(exp, upper)
        }
        _ => {
            // `%g`: P significant digits, where 0 means 1.
            let p = match spec.precision {
                Some(0) => 1,
                Some(p) => p,
                None => 6,
            };
            if v == 0.0 {
                let s = match spec.alt {
                    true => format!("{:.*}", p - 1, 0.0),
                    false => "0".to_owned(),
                };
                return s;
            }
            let (_, x) = exp_parts(v, p - 1);
            let s = if (x as i64) < p as i64 && x >= -4 {
                format!("{:.*}", (p as i64 - 1 - x as i64) as usize, v)
            } else {
                let (mantissa, exp) = exp_parts(v, p - 1);
                let mantissa = match spec.alt {
                    true => mantissa,
                    false => strip_zeros(&mantissa),
                };
                return mantissa + &exp_suffix(exp, upper);
            };
            match spec.alt {
                true => s,
                false => strip_zeros(&s),
            }
        }
    }
}

fn pad(body: String, sign: &str, spec: &Spec, numeric: bool) -> String {
    let len = sign.len() + body.len();
    if len >= spec.width {
        return format!("{sign}{body}");
    }
    let fill = spec.width - len;
    if spec.left {
        format!("{sign}{body}{}", " ".repeat(fill))
    } else if spec.zero && numeric {
        format!("{sign}{}{body}", "0".repeat(fill))
    } else {
        format!("{}{sign}{body}", " ".repeat(fill))
    }
}

/// One float conversion (`f`, `e`, `E`, `g`, `G`).
pub fn format_float(v: f64, conv: u8, spec: &Spec) -> String {
    let negative = v.is_sign_negative() && !(v.is_nan());
    let sign = if negative {
        "-"
    } else if spec.plus {
        "+"
    } else if spec.space {
        " "
    } else {
        ""
    };
    let magnitude = v.abs();
    let upper = conv.is_ascii_uppercase();
    let body = if magnitude.is_infinite() {
        if upper { "INF" } else { "inf" }.to_owned()
    } else if magnitude.is_nan() {
        if upper { "NAN" } else { "nan" }.to_owned()
    } else {
        float_body(magnitude, conv, spec)
    };
    pad(body, sign, spec, magnitude.is_finite())
}

/// `%g` with a precision and nothing else — `ToString`'s and `PrintObjVal`'s.
pub fn format_g(v: f64, precision: usize, upper: bool) -> String {
    let spec = Spec {
        precision: Some(precision),
        ..Spec::default()
    };
    format_float(v, if upper { b'G' } else { b'g' }, &spec)
}

/// One integer conversion (`d`, `i`, `o`, `u`, `x`, `X`, `c`) of an `int`.
pub fn format_int(v: i32, conv: u8, spec: &Spec) -> Vec<u8> {
    if conv == b'c' {
        let body = vec![v as u8];
        let fill = spec.width.saturating_sub(1);
        let mut out = Vec::new();
        if !spec.left {
            out.extend(std::iter::repeat_n(b' ', fill));
        }
        out.extend(body);
        if spec.left {
            out.extend(std::iter::repeat_n(b' ', fill));
        }
        return out;
    }
    let (digits, sign, prefix) = match conv {
        b'd' | b'i' => {
            let sign = if v < 0 {
                "-"
            } else if spec.plus {
                "+"
            } else if spec.space {
                " "
            } else {
                ""
            };
            (v.unsigned_abs().to_string(), sign, "")
        }
        b'o' => {
            let d = format!("{:o}", v as u32);
            let prefix = if spec.alt && !d.starts_with('0') { "0" } else { "" };
            (d, "", prefix)
        }
        b'u' => ((v as u32).to_string(), "", ""),
        b'x' => (format!("{:x}", v as u32), "", if spec.alt && v != 0 { "0x" } else { "" }),
        b'X' => (format!("{:X}", v as u32), "", if spec.alt && v != 0 { "0X" } else { "" }),
        _ => (v.to_string(), "", ""),
    };
    // A precision on an integer is a minimum digit count, and it switches off
    // the `0` flag.
    let (digits, zero_ok) = match spec.precision {
        Some(p) => {
            let d = if p == 0 && v == 0 { String::new() } else { digits };
            let d = if d.len() < p { format!("{}{}", "0".repeat(p - d.len()), d) } else { d };
            (d, false)
        }
        None => (digits, true),
    };
    let spec = Spec {
        zero: spec.zero && zero_ok,
        ..*spec
    };
    let prefixed_sign = format!("{sign}{prefix}");
    pad(digits, &prefixed_sign, &spec, true).into_bytes()
}

/// `%s` — bytes, truncated to the precision and padded to the width.
pub fn format_str(s: &[u8], spec: &Spec) -> Vec<u8> {
    let body = match spec.precision {
        Some(p) if p < s.len() => &s[..p],
        _ => s,
    };
    let fill = spec.width.saturating_sub(body.len());
    let mut out = Vec::with_capacity(body.len() + fill);
    if !spec.left {
        out.extend(std::iter::repeat_n(b' ', fill));
    }
    out.extend_from_slice(body);
    if spec.left {
        out.extend(std::iter::repeat_n(b' ', fill));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(v: f64) -> String {
        format_g(v, 6, false)
    }

    #[test]
    fn percent_g_matches_the_c_library() {
        assert_eq!(g(0.0), "0");
        assert_eq!(g(1.0), "1");
        assert_eq!(g(0.5), "0.5");
        assert_eq!(g(100.0), "100");
        assert_eq!(g(123456.0), "123456");
        assert_eq!(g(1234567.0), "1.23457e+06");
        assert_eq!(g(0.0001), "0.0001");
        assert_eq!(g(0.00001), "1e-05");
        assert_eq!(g(-2.5), "-2.5");
        assert_eq!(g(0.1f32 as f64), "0.1");
        assert_eq!(g(1e20), "1e+20");
        assert_eq!(g(999999.5), "1e+06");
        assert_eq!(format_g(0.1f32 as f64, 14, false), "0.10000000149012");
    }

    #[test]
    fn the_other_conversions_match_the_c_library() {
        let spec = Spec::default();
        assert_eq!(format_float(3.14159, b'f', &spec), "3.141590");
        assert_eq!(format_float(3.14159, b'e', &spec), "3.141590e+00");
        let two = Spec { precision: Some(2), ..Spec::default() };
        assert_eq!(format_float(2.0 / 3.0, b'f', &two), "0.67");
        let wide = Spec { width: 8, zero: true, ..Spec::default() };
        assert_eq!(String::from_utf8(format_int(-42, b'd', &wide)).unwrap(), "-0000042");
        assert_eq!(String::from_utf8(format_int(255, b'x', &Spec::default())).unwrap(), "ff");
        assert_eq!(String::from_utf8(format_int(-1, b'u', &Spec::default())).unwrap(), "4294967295");
        let left = Spec { width: 5, left: true, ..Spec::default() };
        assert_eq!(String::from_utf8(format_str(b"ab", &left)).unwrap(), "ab   ");
    }
}
