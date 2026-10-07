//! Validate scalar safety before `serde_json` can turn YAML NaN into null.
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use std::fmt;
struct Safe;
struct Scalars;
impl<'de> Deserialize<'de> for Safe {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(Scalars)
    }
}
impl<'de> Visitor<'de> for Scalars {
    type Value = Safe;
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("finite JSON-compatible scalars")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Safe, E> {
        Ok(Safe)
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Safe, E> {
        Ok(Safe)
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Safe, E> {
        Ok(Safe)
    }
    fn visit_i128<E: de::Error>(self, _: i128) -> Result<Safe, E> {
        Ok(Safe)
    }
    fn visit_u128<E: de::Error>(self, _: u128) -> Result<Safe, E> {
        Ok(Safe)
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Safe, E> {
        if value.is_finite() {
            Ok(Safe)
        } else {
            Err(E::custom("nonfinite number"))
        }
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Safe, E> {
        if value.contains('\0') {
            Err(E::custom("NUL scalar"))
        } else {
            Ok(Safe)
        }
    }
    fn visit_string<E: de::Error>(self, value: String) -> Result<Safe, E> {
        self.visit_str(&value)
    }
    fn visit_unit<E: de::Error>(self) -> Result<Safe, E> {
        Ok(Safe)
    }
    fn visit_none<E: de::Error>(self) -> Result<Safe, E> {
        Ok(Safe)
    }
    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Safe, D::Error> {
        Safe::deserialize(deserializer)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut values: A) -> Result<Safe, A::Error> {
        while values.next_element::<Safe>()?.is_some() {}
        Ok(Safe)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut values: A) -> Result<Safe, A::Error> {
        while let Some(key) = values.next_key::<String>()? {
            if key.contains('\0') {
                return Err(de::Error::custom("NUL key"));
            }
            values.next_value::<Safe>()?;
        }
        Ok(Safe)
    }
}
pub(crate) fn validate(
    text: &str,
    options: serde_saphyr::Options,
) -> Result<(), serde_saphyr::Error> {
    serde_saphyr::from_str_with_options::<Safe>(text, options).map(|_| ())
}
