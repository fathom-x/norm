//! String enums that tolerate values this SDK version doesn't know.

/// Declares a string-valued enum with an `Other(String)` catch-all, so a
/// value the API adds later deserializes instead of failing the response.
macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $($variant:ident = $value:literal,)* }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum $name {
            $(#[doc = concat!("`", $value, "`")] $variant,)*
            /// A value this SDK version doesn't know.
            Other(String),
        }

        impl $name {
            /// Every value this SDK version knows, as sent on the wire.
            pub const KNOWN: &'static [&'static str] = &[$($value,)*];

            /// The wire value.
            #[must_use]
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $value,)*
                    Self::Other(value) => value,
                }
            }

            /// False for [`Self::Other`].
            #[must_use]
            pub fn is_known(&self) -> bool {
                !matches!(self, Self::Other(_))
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                match value {
                    $($value => Self::$variant,)*
                    other => Self::Other(other.to_string()),
                }
            }
        }

        impl std::str::FromStr for $name {
            type Err = std::convert::Infallible;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Ok(Self::from(value))
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.as_str() == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.as_str() == *other
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let value = String::deserialize(d)?;
                Ok(Self::from(value.as_str()))
            }
        }
    };
}

pub(crate) use string_enum;
