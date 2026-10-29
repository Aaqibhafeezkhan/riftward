//! Formulas: amounts written in data as small expressions, `"roll(2, 5) + power / 3"`, parsed when
//! the file loads and worked out when they're used (`docs/effects.md#formulas`).
//!
//! - **Numbers** and **dice**: `3`, `0.5`, `2d6` (two six-sided dice, added), and `roll(2, 5)`, one
//!   roll from 2 to 5. Every roll goes through `Dice`, with the user's Luck.
//! - **Inputs** by name ([`Inputs`]): `power`, `strength` (a condition's), `margin`, `level`,
//!   `user.<attribute>` and `target.<attribute>` (`user.strength`, `target.willpower`), and
//!   `user.health`, `user.most_health`, `target.health`, `target.most_health`.
//! - **Operators** `+ - * /` (`/` doesn't round), brackets, and `min(a, b)`, `max(a, b)`,
//!   `floor(x)`, `ceil(x)`, `round(x)`.
//!
//! An amount is rounded to a whole number where it's used. There are no loops or assignments:
//! anything that needs more is a script.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::attributes::Attribute;
use crate::dice::Dice;

/// A parsed formula, and the text it came from.
#[derive(Clone, PartialEq)]
pub struct Formula {
    text: String,
    expr: Expr,
}

#[derive(Clone, Debug, PartialEq)]
enum Expr {
    Number(f64),
    /// `NdM`: N dice of M sides, added.
    Dice(u32, u32),
    Input(String),
    Neg(Box<Expr>),
    Op(char, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

/// The names a formula can use besides attributes.
const INPUTS: &[&str] = &["power", "strength", "margin", "level", "age", "allies"];
/// What `user.` and `target.` can name besides attributes.
const VITALS: &[&str] = &["health", "most_health", "coins"];
/// What `target.` can name besides a character's: on a lock (a door or container), how hard it is
/// to pick and to force; on an item, what it's worth; on a room, how hard its ground is to read.
const THINGS: &[&str] = &["pick", "sturdiness", "value", "ground"];
/// Functions, and how many arguments each takes.
const FUNCTIONS: &[(&str, usize)] = &[
    ("min", 2),
    ("max", 2),
    ("floor", 1),
    ("ceil", 1),
    ("round", 1),
    ("roll", 2),
    ("log10", 1),
];

/// What a formula's inputs are worth where it's used. Anything not given counts as nothing,
/// but `strength`, which is 1 unless a condition says otherwise.
#[derive(Clone, Debug, Default)]
pub struct Inputs {
    pub power: f64,
    pub strength: Option<f64>,
    pub margin: f64,
    pub level: f64,
    /// How old what's being read is, in real minutes: a track's age.
    pub age: f64,
    /// How many of the user's allies are fighting in the room with them: a wolf's pack.
    pub allies: f64,
    pub user: Side,
    pub target: Side,
}

/// One side's attributes and health, or for a lock, how hard it is to pick and to force.
#[derive(Clone, Debug, Default)]
pub struct Side {
    pub attributes: Vec<(Attribute, f64)>,
    pub health: f64,
    pub most_health: f64,
    pub pick: f64,
    pub sturdiness: f64,
    /// The coins in their purse, in copper.
    pub coins: f64,
    /// An item's worth, in copper; nothing for what no merchant would pay for.
    pub value: f64,
    /// How hard a room's ground is to read a fresh track on.
    pub ground: f64,
}

impl Side {
    fn get(&self, name: &str) -> f64 {
        match name {
            "health" => self.health,
            "most_health" => self.most_health,
            "pick" => self.pick,
            "coins" => self.coins,
            "value" => self.value,
            "ground" => self.ground,
            "sturdiness" => self.sturdiness,
            _ => self
                .attributes
                .iter()
                .find(|(a, _)| a.name().eq_ignore_ascii_case(name))
                .map_or(0.0, |(_, v)| *v),
        }
    }
}

impl Inputs {
    fn get(&self, name: &str) -> f64 {
        match name.split_once('.') {
            Some(("user", rest)) => self.user.get(rest),
            Some(("target", rest)) => self.target.get(rest),
            _ => match name {
                "power" => self.power,
                "strength" => self.strength.unwrap_or(1.0),
                "margin" => self.margin,
                "level" => self.level,
                "age" => self.age,
                "allies" => self.allies,
                _ => 0.0,
            },
        }
    }
}

impl Formula {
    pub fn parse(text: &str) -> Result<Self, String> {
        let tokens = lex(text)?;
        let mut parser = Parser { tokens, at: 0 };
        let expr = parser.expr()?;
        if parser.at < parser.tokens.len() {
            return Err(format!(
                "'{text}': unexpected {:?}",
                parser.tokens[parser.at]
            ));
        }
        Ok(Self {
            text: text.trim().to_string(),
            expr,
        })
    }

    /// A formula that's just a number.
    pub fn number(n: f64) -> Self {
        Self {
            text: n.to_string(),
            expr: Expr::Number(n),
        }
    }

    /// Work it out, rolling any dice with `luck`.
    pub fn eval(&self, inputs: &Inputs, dice: &mut Dice, luck: i32) -> f64 {
        eval(&self.expr, inputs, dice, luck)
    }

    /// Work it out, rounded to a whole number.
    pub fn amount(&self, inputs: &Inputs, dice: &mut Dice, luck: i32) -> i32 {
        self.eval(inputs, dice, luck).round() as i32
    }

    /// The same formula with `strength` fixed at `n`: an item condition's effects as they ride
    /// on its weapon.
    /// Whether it reads the input `name` (`"target.pick"`).
    pub fn reads(&self, name: &str) -> bool {
        fn reads(expr: &Expr, name: &str) -> bool {
            match expr {
                Expr::Input(input) => input == name,
                Expr::Number(_) | Expr::Dice(..) => false,
                Expr::Neg(e) => reads(e, name),
                Expr::Op(_, a, b) => reads(a, name) || reads(b, name),
                Expr::Call(_, args) => args.iter().any(|a| reads(a, name)),
            }
        }
        reads(&self.expr, name)
    }

    pub fn with_strength(&self, n: f64) -> Self {
        Self {
            text: self.text.clone(),
            expr: bind(&self.expr, "strength", n),
        }
    }
}

fn bind(expr: &Expr, name: &str, value: f64) -> Expr {
    let bound = |e: &Expr| Box::new(bind(e, name, value));
    match expr {
        Expr::Input(n) if n == name => Expr::Number(value),
        Expr::Neg(e) => Expr::Neg(bound(e)),
        Expr::Op(op, a, b) => Expr::Op(*op, bound(a), bound(b)),
        Expr::Call(f, args) => Expr::Call(
            f.clone(),
            args.iter().map(|a| bind(a, name, value)).collect(),
        ),
        other => other.clone(),
    }
}

fn eval(expr: &Expr, inputs: &Inputs, dice: &mut Dice, luck: i32) -> f64 {
    let mut go = |e: &Expr| eval(e, inputs, dice, luck);
    match expr {
        Expr::Number(n) => *n,
        Expr::Dice(count, sides) => (0..*count)
            .map(|_| dice.range(luck, 1..=i64::from((*sides).max(1))) as f64)
            .sum(),
        Expr::Input(name) => inputs.get(name),
        Expr::Neg(e) => -go(e),
        Expr::Op(op, a, b) => {
            let (a, b) = (go(a), go(b));
            match op {
                '+' => a + b,
                '-' => a - b,
                '*' => a * b,
                _ if b == 0.0 => 0.0,
                _ => a / b,
            }
        }
        Expr::Call(f, args) => {
            let args: Vec<f64> = args.iter().map(&mut go).collect();
            match (f.as_str(), args.as_slice()) {
                ("min", [a, b]) => a.min(*b),
                ("max", [a, b]) => a.max(*b),
                ("floor", [x]) => x.floor(),
                ("ceil", [x]) => x.ceil(),
                ("round", [x]) => x.round(),
                // Of nothing or less, nothing.
                ("log10", [x]) if *x > 0.0 => x.log10(),
                ("log10", _) => 0.0,
                ("roll", [a, b]) => {
                    let (low, high) = (a.min(*b).round() as i64, a.max(*b).round() as i64);
                    dice.range(luck, low..=high) as f64
                }
                _ => 0.0,
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    Dice(u32, u32),
    Name(String),
    Symbol(char),
}

fn lex(text: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || c == '.' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let number: String = chars[start..i].iter().collect();
            // `2d6`: dice.
            if i < chars.len()
                && chars[i] == 'd'
                && chars.get(i + 1).is_some_and(char::is_ascii_digit)
            {
                i += 1;
                let sides_start = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let sides: String = chars[sides_start..i].iter().collect();
                let count = number
                    .parse()
                    .map_err(|_| format!("'{text}': '{number}' isn't a count of dice"))?;
                tokens.push(Token::Dice(count, sides.parse().unwrap_or(1)));
            } else {
                let n = number
                    .parse()
                    .map_err(|_| format!("'{text}': '{number}' isn't a number"))?;
                tokens.push(Token::Number(n));
            }
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '.')
            {
                i += 1;
            }
            tokens.push(Token::Name(chars[start..i].iter().collect()));
        } else if "+-*/(),".contains(c) {
            tokens.push(Token::Symbol(c));
            i += 1;
        } else {
            return Err(format!(
                "'{text}': '{c}' isn't something a formula can have"
            ));
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn symbol(&mut self, c: char) -> bool {
        if self.peek() == Some(&Token::Symbol(c)) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expr(&mut self) -> Result<Expr, String> {
        let mut left = self.term()?;
        loop {
            let op = if self.symbol('+') {
                '+'
            } else if self.symbol('-') {
                '-'
            } else {
                return Ok(left);
            };
            left = Expr::Op(op, Box::new(left), Box::new(self.term()?));
        }
    }

    fn term(&mut self) -> Result<Expr, String> {
        let mut left = self.factor()?;
        loop {
            let op = if self.symbol('*') {
                '*'
            } else if self.symbol('/') {
                '/'
            } else {
                return Ok(left);
            };
            left = Expr::Op(op, Box::new(left), Box::new(self.factor()?));
        }
    }

    fn factor(&mut self) -> Result<Expr, String> {
        if self.symbol('-') {
            return Ok(Expr::Neg(Box::new(self.factor()?)));
        }
        if self.symbol('(') {
            let inner = self.expr()?;
            return if self.symbol(')') {
                Ok(inner)
            } else {
                Err("a '(' is never closed".into())
            };
        }
        let token = self.peek().cloned().ok_or("it ends too soon")?;
        self.at += 1;
        match token {
            Token::Number(n) => Ok(Expr::Number(n)),
            Token::Dice(count, sides) => Ok(Expr::Dice(count, sides)),
            Token::Name(name) if self.symbol('(') => {
                let Some(&(_, arity)) = FUNCTIONS.iter().find(|(f, _)| *f == name) else {
                    return Err(format!("there's no function '{name}'"));
                };
                let mut args = Vec::new();
                if !self.symbol(')') {
                    loop {
                        args.push(self.expr()?);
                        if self.symbol(')') {
                            break;
                        }
                        if !self.symbol(',') {
                            return Err(format!("'{name}(' is never closed"));
                        }
                    }
                }
                if args.len() != arity {
                    return Err(format!("{name} takes {arity}, not {}", args.len()));
                }
                Ok(Expr::Call(name, args))
            }
            Token::Name(name) => {
                known(&name)?;
                Ok(Expr::Input(name))
            }
            Token::Symbol(c) => Err(format!("'{c}' where a number should be")),
        }
    }
}

/// Whether `name` is an input formulas know.
fn known(name: &str) -> Result<(), String> {
    let ok = match name.split_once('.') {
        Some(("target", rest)) if THINGS.contains(&rest) => true,
        Some(("user" | "target", rest)) => {
            VITALS.contains(&rest)
                || Attribute::ALL
                    .iter()
                    .any(|a| a.name().eq_ignore_ascii_case(rest))
        }
        Some(_) => false,
        None => INPUTS.contains(&name),
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "there's no input '{name}': use {}, or user. and target. with an attribute, health, \
             most_health or coins, target.pick or target.sturdiness for a lock, \
             target.value for an item, target.ground for a room",
            INPUTS.join(", ")
        ))
    }
}

impl fmt::Debug for Formula {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.text)
    }
}

impl fmt::Display for Formula {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// Written in data as a string, `"roll(2, 5) + power / 3"`, or as a plain number, `4`.
impl<'de> Deserialize<'de> for Formula {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Written {
            Number(f64),
            Text(String),
        }
        match Written::deserialize(deserializer)? {
            Written::Number(n) => Ok(Formula::number(n)),
            Written::Text(text) => Formula::parse(&text).map_err(serde::de::Error::custom),
        }
    }
}

impl Serialize for Formula {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worked(text: &str, inputs: &Inputs) -> f64 {
        Formula::parse(text)
            .unwrap()
            .eval(inputs, &mut Dice::seeded(1), 0)
    }

    #[test]
    fn formulas_do_arithmetic_with_their_inputs() {
        let inputs = Inputs {
            power: 9.0,
            user: Side {
                attributes: vec![(Attribute::Strength, 4.0)],
                health: 10.0,
                most_health: 20.0,
                ..Side::default()
            },
            target: Side {
                pick: 6.0,
                ..Side::default()
            },
            ..Inputs::default()
        };
        assert_eq!(worked("4 + power / 3", &inputs), 7.0);
        assert_eq!(worked("(1 + 2) * 3 - -1", &inputs), 10.0);
        assert_eq!(worked("max(1, user.strength - 6)", &inputs), 1.0);
        assert_eq!(worked("floor(user.health / 3)", &inputs), 3.0);
        assert_eq!(worked("user.most_health", &inputs), 20.0);
        assert_eq!(
            worked("strength * 2", &inputs),
            2.0,
            "strength is 1 unless set"
        );
        assert_eq!(
            worked("target.willpower", &inputs),
            0.0,
            "missing counts as nothing"
        );
        assert_eq!(worked("target.pick + 1", &inputs), 7.0, "a lock's numbers");
        assert_eq!(worked("log10(1000)", &inputs), 3.0);
        assert_eq!(worked("log10(0)", &inputs), 0.0, "of nothing, nothing");
        let pick = Formula::parse("max(1, target.pick - power)").unwrap();
        assert!(pick.reads("target.pick"));
        assert!(!pick.reads("target.sturdiness"));
        assert_eq!(
            worked("power / 0", &inputs),
            0.0,
            "never a division by zero"
        );
    }

    #[test]
    fn dice_roll_within_their_range() {
        let inputs = Inputs::default();
        let mut dice = Dice::seeded(3);
        let two_d_six = Formula::parse("2d6").unwrap();
        let roll = Formula::parse("roll(2, 5)").unwrap();
        for _ in 0..200 {
            assert!((2.0..=12.0).contains(&two_d_six.eval(&inputs, &mut dice, 0)));
            assert!((2.0..=5.0).contains(&roll.eval(&inputs, &mut dice, 0)));
        }
    }

    #[test]
    fn mistakes_are_caught_when_parsed() {
        for bad in [
            "power +",
            "pwoer",
            "user.strenght",
            "max(1)",
            "sqrt(4)",
            "2 $ 3",
            "(1 + 2",
        ] {
            assert!(Formula::parse(bad).is_err(), "{bad} should fail");
        }
    }

    #[test]
    fn strength_can_be_fixed() {
        let f = Formula::parse("roll(1, 3) * strength").unwrap();
        let hot = f.with_strength(2.0);
        let mut dice = Dice::seeded(5);
        for _ in 0..50 {
            let n = hot.eval(&Inputs::default(), &mut dice, 0);
            assert!((2.0..=6.0).contains(&n) && n % 2.0 == 0.0);
        }
    }

    #[test]
    fn formulas_are_read_as_strings_or_numbers() {
        let f: Formula = ron::from_str("\"roll(2, 5) + power / 3\"").unwrap();
        assert_eq!(f.to_string(), "roll(2, 5) + power / 3");
        let n: Formula = ron::from_str("4").unwrap();
        assert_eq!(n.eval(&Inputs::default(), &mut Dice::seeded(1), 0), 4.0);
        assert!(ron::from_str::<Formula>("\"pwoer\"").is_err());
    }
}
