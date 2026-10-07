//! Canonical evidence bytes shared by API persistence and worker verification.
use super::{Document, DocumentBuilder, Node, NodeId};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{fmt::Write as _, io};

fn copy(
    document: &Document,
    id: NodeId,
    builder: &mut DocumentBuilder,
    depth: usize,
    budget: usize,
) -> Result<NodeId, &'static str> {
    if depth >= budget {
        return Err("canonical document exceeds nesting limit");
    }
    let node = match document.node(id).ok_or("missing canonical node")? {
        Node::Object(fields) => {
            let mut fields = fields.clone();
            fields.sort_by_key(|(left, _)| left.codepoints());
            Node::Object(
                fields
                    .into_iter()
                    .map(|(key, child)| {
                        Ok((key, copy(document, child, builder, depth + 1, budget)?))
                    })
                    .collect::<Result<_, &'static str>>()?,
            )
        }
        Node::Array(children) => Node::Array(
            children
                .iter()
                .map(|child| copy(document, *child, builder, depth + 1, budget))
                .collect::<Result<_, _>>()?,
        ),
        Node::String(value) => Node::String(value.clone()),
        Node::Integer(value) => Node::Integer(value.clone()),
        Node::Float(value) => Node::Float(*value),
        Node::Bool(value) => Node::Bool(*value),
        Node::Null => Node::Null,
    };
    builder.push(node).map_err(|_| "invalid canonical node")
}

/// Sorted compact UTF-8 JSON retains integer versus float representation.
/// # Errors
/// Rejects invalid nodes, nonfinite numbers, invalid UTF-8 text and excessive depth.
pub fn bytes(document: &Document, budget: usize) -> Result<Vec<u8>, &'static str> {
    let mut builder = DocumentBuilder::new();
    let root = copy(document, document.root(), &mut builder, 0, budget)?;
    let sorted = builder.finish(root).map_err(|_| "invalid canonical root")?;
    let value = super::node_value(&sorted, sorted.root(), budget)
        .map_err(|_| "canonical rendering failed")?;
    let mut output = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut output, EvidenceFormatter);
    value
        .serialize(&mut serializer)
        .map_err(|_| "canonical rendering failed")?;
    Ok(output)
}

/// SHA-256 of canonical evidence bytes.
/// # Errors
/// Returns the same representation and depth errors as [`bytes`].
pub fn sha256(document: &Document, budget: usize) -> Result<String, &'static str> {
    Ok(format!("{:x}", Sha256::digest(bytes(document, budget)?)))
}

#[allow(unused_imports)]
use crate::text::TextExt as _;

/// Version-1 evidence protocol's binary64 spelling, shared with deployed evaluators.
/// Shortest decimal selection uses zmij; this adapter only changes notation.
#[must_use]
fn canonical_float_text(value: f64) -> String {
    if value.is_nan() {
        return "nan".into();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-inf"
        } else {
            "inf"
        }
        .into();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .into();
    }
    let mut buffer = zmij::Buffer::new();
    let formatted = buffer.format_finite(value);
    let unsigned = formatted.strip_prefix('-').unwrap_or(formatted);
    let (mantissa, raw_exponent) = unsigned.split_once('e').unwrap_or((unsigned, "0"));
    // These are bounded ASCII parts produced by the vetted finite formatter,
    // not user input. No rounding or decimal-to-binary conversion occurs here.
    let exponent_negative = raw_exponent.starts_with('-');
    let mut exponent = 0_i32;
    for byte in raw_exponent.bytes().filter(u8::is_ascii_digit) {
        exponent = exponent * 10 + i32::from(byte - b'0');
    }
    if exponent_negative {
        exponent = -exponent;
    }
    let mut decimal_position = 0_i32;
    for character in mantissa.chars().take_while(|&c| c != '.') {
        let _ = character;
        decimal_position += 1;
    }
    let mut digits = String::new();
    let mut leading_zeroes = 0_i32;
    for character in mantissa.chars().filter(|&c| c != '.') {
        if digits.is_empty() && character == '0' {
            leading_zeroes += 1;
        } else {
            digits.push(character);
        }
    }
    while digits.ends_with('0') {
        digits.pop();
    }
    let scientific_exponent = exponent + decimal_position - leading_zeroes - 1;
    let mut output = String::new();
    if value.is_sign_negative() {
        output.push('-');
    }
    if (-4..=15).contains(&scientific_exponent) {
        let point = scientific_exponent + 1;
        if point <= 0 {
            output.push_str("0.");
            for _ in 0..-point {
                output.push('0');
            }
            output.push_str(&digits);
        } else {
            let mut written = 0_i32;
            for character in digits.chars() {
                if written == point {
                    output.push('.');
                }
                output.push(character);
                written += 1;
            }
            if written <= point {
                for _ in written..point {
                    output.push('0');
                }
                output.push_str(".0");
            }
        }
    } else {
        for (index, character) in digits.chars().enumerate() {
            if index == 1 {
                output.push('.');
            }
            output.push(character);
        }
        let _ = write!(output, "e{scientific_exponent:+03}");
    }
    output
}

struct EvidenceFormatter;
impl serde_json::ser::Formatter for EvidenceFormatter {
    fn write_f64<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        writer.write_all(canonical_float_text(value).as_bytes())
    }
    fn write_number_str<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        value: &str,
    ) -> io::Result<()> {
        if value.contains(['.', 'e', 'E']) {
            let number = value
                .parse::<f64>()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            self.write_f64(writer, number)
        } else {
            writer.write_all(value.as_bytes())
        }
    }
}
