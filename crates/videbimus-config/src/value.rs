//! Resolved values: numbers, strings, booleans and times with units, and
//! the timebase that turns times into frames.

use std::fmt;

use videbimus_core::{Frame, Rate, Tempo};

/// A unit of time, written as a KDL type annotation (`(bar)4`) or as a
/// suffix inside an expression (`4bar`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unit {
    /// Bars of the song's tempo: `bar`.
    Bar,
    /// Beats of the song's tempo: `beat`.
    Beat,
    /// Seconds: `s`.
    Second,
    /// Frames at the video's rate: `f`.
    Frame,
}

impl Unit {
    /// Every unit, in the order messages list them.
    pub const ALL: [Unit; 4] = [Unit::Bar, Unit::Beat, Unit::Second, Unit::Frame];

    /// The unit written `suffix`: `bar`, `beat`, `s` or `f`.
    #[must_use]
    pub fn from_suffix(suffix: &str) -> Option<Unit> {
        Unit::ALL.into_iter().find(|u| u.suffix() == suffix)
    }

    /// How it is written.
    #[must_use]
    pub fn suffix(self) -> &'static str {
        match self {
            Unit::Bar => "bar",
            Unit::Beat => "beat",
            Unit::Second => "s",
            Unit::Frame => "f",
        }
    }

    fn is_musical(self) -> bool {
        matches!(self, Unit::Bar | Unit::Beat)
    }
}

/// A time: an amount of a unit, measured from the start of the video (bar
/// 0, second 0, frame 0).
///
/// Positions and durations share this type. With one constant tempo they
/// convert to frames identically, and the resolver never rounds before the
/// final conversion, so every boundary still snaps once from its own
/// position (ADR 0001 §4). See ADR 0005 for the rules.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Time {
    /// How many units.
    pub amount: f64,
    /// Which unit.
    pub unit: Unit,
}

impl Time {
    /// `amount` of `unit`.
    #[must_use]
    pub fn new(amount: f64, unit: Unit) -> Self {
        Self { amount, unit }
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.amount, self.unit.suffix())
    }
}

/// A resolved value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `#null`.
    Null,
    /// `#true` or `#false`.
    Bool(bool),
    /// A number without a unit. KDL integers and floats both become
    /// numbers; integers are exact up to 2⁵³.
    Number(f64),
    /// A string.
    String(String),
    /// A time with a unit.
    Time(Time),
}

impl Value {
    /// The number, if it is one.
    #[must_use]
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// The string, if it is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The boolean, if it is one.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The time, if it is one.
    #[must_use]
    pub fn as_time(&self) -> Option<Time> {
        match self {
            Value::Time(t) => Some(*t),
            _ => None,
        }
    }

    /// What sort of value it is, for messages: "a number", "a time".
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Value::Null => "#null".into(),
            Value::Bool(_) => format!("the boolean {self}"),
            Value::Number(_) => format!("the number {self}"),
            Value::String(_) => format!("the string {self}"),
            Value::Time(_) => format!("the time {self}"),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => f.write_str("#null"),
            Value::Bool(true) => f.write_str("#true"),
            Value::Bool(false) => f.write_str("#false"),
            Value::Number(n) => write!(f, "{n}"),
            Value::String(s) => write!(f, "{s:?}"),
            Value::Time(t) => write!(f, "{t}"),
        }
    }
}

/// The video's frame rate, and the song's tempo if it has one: what times
/// in bars, beats and seconds are converted to frames with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timebase {
    /// Frames per second.
    pub rate: Rate,
    /// The song's tempo, if the config gives one.
    pub tempo: Option<Tempo>,
}

/// A time in bars or beats met a timebase with no tempo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoTempo;

impl fmt::Display for NoTempo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bars and beats need the song's tempo, and the config gives none")
    }
}

impl std::error::Error for NoTempo {}

impl Timebase {
    fn tempo(&self) -> Result<Tempo, NoTempo> {
        self.tempo.ok_or(NoTempo)
    }

    /// `t` in seconds from the start, exactly (no rounding).
    ///
    /// # Errors
    ///
    /// If `t` is in bars or beats and there is no tempo.
    pub fn seconds(&self, t: Time) -> Result<f64, NoTempo> {
        Ok(match t.unit {
            Unit::Bar => self.tempo()?.seconds(t.amount, 0.0),
            Unit::Beat => self.tempo()?.seconds(0.0, t.amount),
            Unit::Second => t.amount,
            Unit::Frame => t.amount * f64::from(self.rate.den()) / f64::from(self.rate.num()),
        })
    }

    /// `t` in `unit`, exactly (no rounding).
    ///
    /// # Errors
    ///
    /// If either unit is musical and there is no tempo.
    pub fn convert(&self, t: Time, unit: Unit) -> Result<Time, NoTempo> {
        if t.unit == unit {
            return Ok(t);
        }
        let amount = if t.unit.is_musical() && unit.is_musical() {
            let bpb = f64::from(self.tempo()?.beats_per_bar);
            if unit == Unit::Beat {
                t.amount * bpb
            } else {
                t.amount / bpb
            }
        } else {
            let s = self.seconds(t)?;
            match unit {
                Unit::Bar => {
                    let tempo = self.tempo()?;
                    s * tempo.bpm / 60.0 / f64::from(tempo.beats_per_bar)
                }
                Unit::Beat => s * self.tempo()?.bpm / 60.0,
                Unit::Second => s,
                Unit::Frame => s * f64::from(self.rate.num()) / f64::from(self.rate.den()),
            }
        };
        Ok(Time::new(amount, unit))
    }

    /// The nearest frame to `t`, converted from its own position (ADR 0001
    /// §4): bars and beats through [`Tempo::frame`], seconds through
    /// [`Rate::frame_at`], frames rounded.
    ///
    /// # Errors
    ///
    /// If `t` is in bars or beats and there is no tempo.
    #[allow(clippy::cast_possible_truncation)]
    pub fn frame(&self, t: Time) -> Result<Frame, NoTempo> {
        Ok(match t.unit {
            Unit::Bar => self.tempo()?.frame(self.rate, t.amount, 0.0),
            Unit::Beat => self.tempo()?.frame(self.rate, 0.0, t.amount),
            Unit::Second => self.rate.frame_at(t.amount),
            Unit::Frame => Frame(t.amount.round() as i64),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEN: Timebase = Timebase {
        rate: Rate::new(30, 1),
        tempo: Some(Tempo {
            bpm: 104.0,
            beats_per_bar: 4,
        }),
    };

    #[test]
    fn conversions() {
        let bar = Time::new(1.0, Unit::Bar);
        assert_eq!(
            FEN.convert(bar, Unit::Beat).unwrap(),
            Time::new(4.0, Unit::Beat)
        );
        let s = FEN.convert(bar, Unit::Second).unwrap().amount;
        assert!((s - 240.0 / 104.0).abs() < 1e-12);
        let f = FEN
            .convert(Time::new(0.5, Unit::Second), Unit::Frame)
            .unwrap();
        assert!((f.amount - 15.0).abs() < 1e-12);
        let back = FEN
            .convert(FEN.convert(bar, Unit::Frame).unwrap(), Unit::Bar)
            .unwrap();
        assert!((back.amount - 1.0).abs() < 1e-12);
        assert_eq!(FEN.frame(Time::new(4.0, Unit::Bar)), Ok(Frame(277)));
        assert_eq!(FEN.frame(Time::new(16.0, Unit::Beat)), Ok(Frame(277)));
        assert_eq!(FEN.frame(Time::new(0.5, Unit::Second)), Ok(Frame(15)));
        let none = Timebase {
            rate: Rate::new(30, 1),
            tempo: None,
        };
        assert_eq!(none.frame(bar), Err(NoTempo));
        assert_eq!(none.frame(Time::new(2.0, Unit::Second)), Ok(Frame(60)));
    }
}
