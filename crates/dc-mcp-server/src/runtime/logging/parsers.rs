use std::{fmt::Display, marker::PhantomData, str::FromStr};

use serde::Deserializer;

pub(crate) fn from_str<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
    <T as FromStr>::Err: Display,
{
    struct FromStrVisitor<Inner> {
        _phantom: PhantomData<Inner>,
    }
    impl<Inner> serde::de::Visitor<'_> for FromStrVisitor<Inner>
    where
        Inner: FromStr,
        <Inner as FromStr>::Err: Display,
    {
        type Value = Inner;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string")
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Inner::from_str(v).map_err(|e| serde::de::Error::custom(e.to_string()))
        }
    }

    deserializer.deserialize_str(FromStrVisitor {
        _phantom: PhantomData,
    })
}
