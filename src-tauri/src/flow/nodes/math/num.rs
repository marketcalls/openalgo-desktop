//! Python's numbers as the evaluator meets them: `int` (exact), `float`
//! and the `complex` a negative base raised to a fractional power becomes,
//! with CPython 3.12's arithmetic rules and error sentences
//! (`Objects/longobject.c`, `floatobject.c`, `complexobject.c`).

use super::bigint::BigInt;
use crate::flow::contract::pyfmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Num {
    Int(BigInt),
    Float(f64),
    Complex(f64, f64),
}

/// What an operation raised: Python's `str()` of the exception.
pub type Raised = String;

/// What Python's float `**` reports on overflow: the C library's ERANGE
/// text, here the web's own (Linux glibc) on every platform (decisions.md
/// 24; macOS and Windows write "Result too large").
pub const FLOAT_POW_OVERFLOW: &str = "(34, 'Numerical result out of range')";

impl Num {
    fn type_name(&self) -> &'static str {
        match self {
            Num::Int(_) => "int",
            Num::Float(_) => "float",
            Num::Complex(..) => "complex",
        }
    }

    /// Python's `str()` of the value.
    pub fn text(&self) -> String {
        match self {
            Num::Int(i) => i.to_decimal(),
            Num::Float(f) => pyfmt::float(*f),
            Num::Complex(re, im) => complex_repr(*re, *im),
        }
    }

    /// The value as a float, as an arithmetic operator converts an operand.
    fn as_float(&self) -> Result<f64, Raised> {
        match self {
            Num::Int(i) => i.to_f64(),
            Num::Float(f) => Ok(*f),
            Num::Complex(..) => Err("can't convert complex to float".into()),
        }
    }

    fn as_complex(&self) -> Result<(f64, f64), Raised> {
        match self {
            Num::Complex(re, im) => Ok((*re, *im)),
            other => Ok((other.as_float()?, 0.0)),
        }
    }

    fn is_complex(&self) -> bool {
        matches!(self, Num::Complex(..))
    }

    /// `float(value)`, the evaluator's last step.
    pub fn to_float(&self) -> Result<f64, Raised> {
        match self {
            Num::Complex(..) => {
                Err("float() argument must be a string or a real number, not 'complex'".into())
            }
            other => other.as_float(),
        }
    }

    /// Whether `abs(self) > 100`, the exponent cap.
    pub fn abs_over(&self, limit: u64) -> Result<bool, Raised> {
        Ok(match self {
            Num::Int(i) => i.abs_cmp_u64(limit) == std::cmp::Ordering::Greater,
            Num::Float(f) => f.abs() > limit as f64,
            Num::Complex(re, im) => {
                let h = re.hypot(*im);
                if h.is_infinite() && re.is_finite() && im.is_finite() {
                    return Err("absolute value too large".into());
                }
                h > limit as f64
            }
        })
    }

    pub fn neg(&self) -> Num {
        match self {
            Num::Int(i) => Num::Int(i.neg()),
            Num::Float(f) => Num::Float(-f),
            Num::Complex(re, im) => Num::Complex(-re, -im),
        }
    }

    pub fn add(&self, o: &Num) -> Result<Num, Raised> {
        if self.is_complex() || o.is_complex() {
            let ((a, b), (c, d)) = (self.as_complex()?, o.as_complex()?);
            return Ok(Num::Complex(a + c, b + d));
        }
        if let (Num::Int(a), Num::Int(b)) = (self, o) {
            return Ok(Num::Int(a.add(b)?));
        }
        Ok(Num::Float(self.as_float()? + o.as_float()?))
    }

    pub fn sub(&self, o: &Num) -> Result<Num, Raised> {
        if self.is_complex() || o.is_complex() {
            let ((a, b), (c, d)) = (self.as_complex()?, o.as_complex()?);
            return Ok(Num::Complex(a - c, b - d));
        }
        if let (Num::Int(a), Num::Int(b)) = (self, o) {
            return Ok(Num::Int(a.sub(b)?));
        }
        Ok(Num::Float(self.as_float()? - o.as_float()?))
    }

    pub fn mul(&self, o: &Num) -> Result<Num, Raised> {
        if self.is_complex() || o.is_complex() {
            let (a, b) = (self.as_complex()?, o.as_complex()?);
            let (re, im) = c_prod(a, b);
            return Ok(Num::Complex(re, im));
        }
        if let (Num::Int(a), Num::Int(b)) = (self, o) {
            return Ok(Num::Int(a.mul(b)?));
        }
        Ok(Num::Float(self.as_float()? * o.as_float()?))
    }

    pub fn div(&self, o: &Num) -> Result<Num, Raised> {
        if self.is_complex() || o.is_complex() {
            let (a, b) = (self.as_complex()?, o.as_complex()?);
            let (q, edom) = c_quot(a, b);
            if edom {
                return Err("complex division by zero".into());
            }
            return Ok(Num::Complex(q.0, q.1));
        }
        if let (Num::Int(a), Num::Int(b)) = (self, o) {
            if b.is_zero() {
                return Err("division by zero".into());
            }
            return Ok(Num::Float(a.true_div(b)?));
        }
        let (a, b) = (self.as_float()?, o.as_float()?);
        if b == 0.0 {
            return Err("float division by zero".into());
        }
        Ok(Num::Float(a / b))
    }

    pub fn rem(&self, o: &Num) -> Result<Num, Raised> {
        if self.is_complex() || o.is_complex() {
            return Err(format!(
                "unsupported operand type(s) for %: '{}' and '{}'",
                self.type_name(),
                o.type_name()
            ));
        }
        if let (Num::Int(a), Num::Int(b)) = (self, o) {
            if b.is_zero() {
                return Err("integer modulo by zero".into());
            }
            return Ok(Num::Int(a.py_mod(b)));
        }
        let (a, b) = (self.as_float()?, o.as_float()?);
        if b == 0.0 {
            return Err("float modulo".into());
        }
        let m = a % b;
        Ok(Num::Float(if m != 0.0 {
            if (b < 0.0) != (m < 0.0) {
                m + b
            } else {
                m
            }
        } else {
            0.0f64.copysign(b)
        }))
    }

    /// `self ** o`, the exponent already within the evaluator's cap.
    pub fn pow(&self, o: &Num) -> Result<Num, Raised> {
        if self.is_complex() || o.is_complex() {
            return complex_pow(self.as_complex()?, o.as_complex()?);
        }
        if let (Num::Int(a), Num::Int(b)) = (self, o) {
            if !b.is_negative() {
                let e = b.to_i64().and_then(|e| u32::try_from(e).ok());
                if let Some(e) = e {
                    return Ok(Num::Int(a.pow(e)?));
                }
            }
        }
        float_pow(self.as_float()?, o.as_float()?)
    }

    /// `math.floor(self)`.
    pub fn floor(&self) -> Result<Num, Raised> {
        match self {
            Num::Int(i) => Ok(Num::Int(i.clone())),
            Num::Float(f) if f.is_nan() => Err("cannot convert float NaN to integer".into()),
            Num::Float(f) if f.is_infinite() => {
                Err("cannot convert float infinity to integer".into())
            }
            Num::Float(f) => Ok(Num::Int(BigInt::from_f64_integral(f.floor()))),
            Num::Complex(..) => Err("must be real number, not complex".into()),
        }
    }
}

/// A float written as complex's repr writes its parts: the shortest
/// round-trip digits without a forced `.0`.
fn short_float(f: f64) -> String {
    let s = pyfmt::float(f);
    match s.strip_suffix(".0") {
        Some(t) => t.to_string(),
        None => s,
    }
}

fn complex_repr(re: f64, im: f64) -> String {
    if re == 0.0 && re.is_sign_positive() {
        return format!("{}j", short_float(im));
    }
    let imag = short_float(im);
    let signed = if imag.starts_with('-') || imag.starts_with('+') {
        imag
    } else {
        format!("+{}", imag)
    };
    format!("({}{}j)", short_float(re), signed)
}

fn c_prod(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}

/// `a / b` (`_Py_c_quot`) and whether it divided by zero.
fn c_quot(a: (f64, f64), b: (f64, f64)) -> ((f64, f64), bool) {
    let abs_br = b.0.abs();
    let abs_bi = b.1.abs();
    if abs_br >= abs_bi {
        if abs_br == 0.0 {
            return ((0.0, 0.0), true);
        }
        let ratio = b.1 / b.0;
        let denom = b.0 + b.1 * ratio;
        (
            ((a.0 + a.1 * ratio) / denom, (a.1 - a.0 * ratio) / denom),
            false,
        )
    } else if abs_bi >= abs_br {
        let ratio = b.0 / b.1;
        let denom = b.0 * ratio + b.1;
        (
            ((a.0 * ratio + a.1) / denom, (a.1 * ratio - a.0) / denom),
            false,
        )
    } else {
        ((f64::NAN, f64::NAN), false)
    }
}

fn c_powu(x: (f64, f64), n: i64) -> (f64, f64) {
    let mut r = (1.0, 0.0);
    let mut p = x;
    let mut mask: i64 = 1;
    while mask > 0 && n >= mask {
        if n & mask != 0 {
            r = c_prod(r, p);
        }
        mask <<= 1;
        p = c_prod(p, p);
    }
    r
}

/// `_Py_c_pow` and whether it raised zero to a negative or complex power.
fn c_pow(a: (f64, f64), b: (f64, f64)) -> ((f64, f64), bool) {
    if b.0 == 0.0 && b.1 == 0.0 {
        return ((1.0, 0.0), false);
    }
    if a.0 == 0.0 && a.1 == 0.0 {
        return ((0.0, 0.0), b.1 != 0.0 || b.0 < 0.0);
    }
    let vabs = a.0.hypot(a.1);
    let mut len = vabs.powf(b.0);
    let at = a.1.atan2(a.0);
    let mut phase = at * b.0;
    if b.1 != 0.0 {
        len /= (at * b.1).exp();
        phase += b.1 * vabs.ln();
    }
    ((len * phase.cos(), len * phase.sin()), false)
}

fn complex_pow(a: (f64, f64), b: (f64, f64)) -> Result<Num, Raised> {
    let (p, edom) = if b.1 == 0.0 && b.0 == b.0.floor() && b.0.abs() <= 100.0 {
        let n = b.0 as i64;
        if n > 0 {
            (c_powu(a, n), false)
        } else {
            c_quot((1.0, 0.0), c_powu(a, -n))
        }
    } else {
        c_pow(a, b)
    };
    if edom {
        return Err("0.0 to a negative or complex power".into());
    }
    if p.0.is_infinite() || p.1.is_infinite() {
        return Err("complex exponentiation".into());
    }
    Ok(Num::Complex(p.0, p.1))
}

fn odd_integer(x: f64) -> bool {
    (x.abs() % 2.0) == 1.0
}

/// `float.__pow__` (`float_pow`), with its special cases before the C
/// library's `pow`.
fn float_pow(iv: f64, iw: f64) -> Result<Num, Raised> {
    if iw == 0.0 {
        return Ok(Num::Float(1.0));
    }
    if iv.is_nan() {
        return Ok(Num::Float(iv));
    }
    if iw.is_nan() {
        return Ok(Num::Float(if iv == 1.0 { 1.0 } else { iw }));
    }
    if iw.is_infinite() {
        let a = iv.abs();
        return Ok(Num::Float(if a == 1.0 {
            1.0
        } else if (iw > 0.0) == (a > 1.0) {
            iw.abs()
        } else {
            0.0
        }));
    }
    if iv.is_infinite() {
        let odd = odd_integer(iw);
        return Ok(Num::Float(if iw > 0.0 {
            if odd {
                iv
            } else {
                iv.abs()
            }
        } else if odd {
            0.0f64.copysign(iv)
        } else {
            0.0
        }));
    }
    if iv == 0.0 {
        if iw < 0.0 {
            return Err("0.0 cannot be raised to a negative power".into());
        }
        return Ok(Num::Float(if odd_integer(iw) { iv } else { 0.0 }));
    }
    let mut base = iv;
    let mut negate = false;
    if iv < 0.0 {
        if iw != iw.floor() {
            return complex_pow((iv, 0.0), (iw, 0.0));
        }
        base = -iv;
        negate = odd_integer(iw);
    }
    if base == 1.0 {
        return Ok(Num::Float(if negate { -1.0 } else { 1.0 }));
    }
    let ix = base.powf(iw);
    if ix.is_infinite() {
        return Err(FLOAT_POW_OVERFLOW.into());
    }
    Ok(Num::Float(if negate { -ix } else { ix }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i(v: i64) -> Num {
        Num::Int(BigInt::from_i64(v))
    }

    #[test]
    fn complex_values_print_as_python() {
        assert_eq!(complex_repr(0.0, 1.0), "1j");
        assert_eq!(complex_repr(0.0, 1e16), "1e+16j");
        assert_eq!(complex_repr(2.0, 0.0), "(2+0j)");
        assert_eq!(complex_repr(-0.0, -1.5), "(-0-1.5j)");
        assert_eq!(complex_repr(1.0, f64::NAN), "(1+nanj)");
    }

    #[test]
    fn mixed_operations_follow_python() {
        assert_eq!(i(7).div(&i(2)), Ok(Num::Float(3.5)));
        assert_eq!(i(7).rem(&Num::Float(0.0)), Err("float modulo".into()));
        assert_eq!(i(2).pow(&i(-1)), Ok(Num::Float(0.5)));
        assert_eq!(
            i(0).pow(&i(-1)),
            Err("0.0 cannot be raised to a negative power".into())
        );
        assert!(matches!(i(-8).pow(&Num::Float(0.5)), Ok(Num::Complex(..))));
        assert_eq!(Num::Float(1e300).pow(&i(2)), Err(FLOAT_POW_OVERFLOW.into()));
        assert_eq!(Num::Float(-7.5).rem(&i(2)), Ok(Num::Float(0.5)));
    }
}
